use core::future::Ready;

use std::future::ready;
use std::str::FromStr;

use tracing::{span, warn, Level};

use domain::base::iana::{Class, Rcode};
use domain::base::message_builder::{AdditionalBuilder, AnswerBuilder};
use domain::base::name::{Label, ToLabelIter};
use domain::base::wire::Composer;
use domain::base::{CharStr, NameBuilder, ParsedName, RelativeName, Rtype, StreamTarget, Ttl};
use domain::net::server::message::Request;
use domain::net::server::service::{CallResult, Service, ServiceError, Transaction};
use domain::net::server::util::mk_builder_for_target;
use domain::rdata::rfc1035::TxtBuilder;

//----------- AgentService ---------------------------------------------------

/// A `Service` impl that acts as an [RFC 9567] error reporting agent.
///
/// [RFC 9567]: https://datatracker.ietf.org/doc/rfc9567/
pub struct AgentService<F>
where
    F: Fn(u16, u16, RelativeName<Vec<u8>>),
{
    // agent_domain: RelativeName?
    /// A user supplied callback function that will handle received reports.
    ///
    /// Will be passed the reported QTYPE, EDNS error code and QNAME as
    /// arguments.
    callback: F,
}

impl<F> AgentService<F>
where
    F: Fn(u16, u16, RelativeName<Vec<u8>>),
{
    /// Creates a new instance of this service.
    pub fn new(callback: F) -> Self {
        Self { callback }
    }

    /// Process an agent request per RFC 9567, if valid.
    ///
    /// Invokes the configured callback to propagate a received error report to
    /// a handling mechanism.
    ///
    /// Returns a DNS TXT response message indicating success or failure.
    fn process_request<Target: Composer + Default>(
        &self,
        request: &Request<Vec<u8>>,
    ) -> AdditionalBuilder<StreamTarget<Target>> {
        // https://www.rfc-editor.org/rfc/rfc9567#section-6.3-1
        // "It is RECOMMENDED that the authoritative server for the agent domain
        // reply with a positive response (i.e., not with NODATA or NXDOMAIN)
        // containing a TXT record."
        let mut response = None;

        if let Ok(question) = request.message().sole_question() {
            // We're expecting an RFC 9567 compatible query, i.e.:
            //   - QTYPE: TXT
            //   - QCLASS: IN?
            //   - QNAME: _er.<decimal qtype>.<query name labels>.<decimal edns
            //     error code>._er.<our agent domain>
            //
            // The QTYPE therefore has at least 6 labels.
            //
            // RFC 9567 doesn't appear to constrain the QCLASS so we will won't
            // check it but one can imagine that it only makes sense for it to be
            // IN.
            //
            // TODO: Should we enforce that the <our agent domain> part of the
            // QNAME matches what we think our agent domain is?
            //
            // See:
            // https://www.rfc-editor.org/rfc/rfc9567#name-constructing-the-report-que
            let qname = question.qname();
            let qtype = question.qtype();
            let num_labels = qname.label_count();

            let span = span!(Level::INFO, "Processing", %qname, %qtype);
            let _enter = span.enter();

            if qtype == Rtype::TXT {
                if num_labels >= 6 {
                    match self.parse_qname(qname) {
                        Err(err) => warn!("QNAME parsing error: {err}"),

                        Ok((rep_qtype, edns_err_code, rep_qname)) => {
                            (self.callback)(rep_qtype, edns_err_code, rep_qname);
                            response = Some(self.mk_success_response(request, qname));
                        }
                    }
                } else {
                    warn!("Insufficient labels in QNAME");
                }
            } else {
                warn!("Invalid QTYPE: {qtype}");
            }
        } else {
            warn!("QDCOUNT != 1");
        }

        if response.is_none() {
            response = Some(self.mk_err_response(request, Rcode::FORMERR));
        }

        response.unwrap().additional()
    }

    /// Parse a QNAME per the RFC 9567 agent query specification.
    ///
    /// Returns Ok((report qtype, report edns error code, report qname)) on
    /// success, Err(String) otherwise.
    fn parse_qname(
        &self,
        qname: &ParsedName<&[u8]>,
    ) -> Result<(u16, u16, RelativeName<Vec<u8>>), String> {
        let mut iter = qname.iter_labels();
        let _er = iter.next().ok_or("Missing _er label.".to_string())?;
        let rep_qtype = iter.next().ok_or("Missing QTYPE label.".to_string())?;
        let mut rep_qname = NameBuilder::new_vec();
        let mut second_last_label = Option::<&Label>::None;
        let mut last_label = None;
        loop {
            let label = iter
                .next()
                .ok_or("Missing QNAME or _er label.".to_string())?;
            if let Some(label) = second_last_label {
                rep_qname
                    .append_label(label.as_slice())
                    .map_err(|err| format!("Invalid QNAME label: {err}"))?;
            }
            if label == "_er" {
                break;
            } else {
                second_last_label = last_label;
                last_label = Some(label);
            }
        }
        let rep_qname = rep_qname.finish();
        let edns_err_code = last_label.ok_or("Missing EDNS error code label.".to_string())?;

        let rep_qtype = u16::from_str(&rep_qtype.to_string())
            .map_err(|err| format!("Invalid QTYPE label: {err}"))?;

        let edns_err_code = u16::from_str(&edns_err_code.to_string())
            .map_err(|err| format!("Invalid EDNS error code label: {err}"))?;

        Ok((rep_qtype, edns_err_code, rep_qname))
    }

    /// Construct an RFC 9567 TXT DNS answer response.
    fn mk_success_response<Target: Composer + Default>(
        &self,
        request: &Request<Vec<u8>>,
        qname: &ParsedName<&[u8]>,
    ) -> AnswerBuilder<StreamTarget<Target>> {
        let builder = mk_builder_for_target();
        let mut answer = builder
            .start_answer(request.message(), Rcode::NOERROR)
            .unwrap();
        let mut txt_builder = TxtBuilder::<Vec<u8>>::new();
        let txt = {
            let cs = CharStr::<Vec<u8>>::from_str("Report received").unwrap();
            txt_builder.append_charstr(&cs).unwrap();
            txt_builder.finish().unwrap()
        };
        answer
            .push((qname, Class::IN, Ttl::from_days(1), txt))
            .unwrap();
        answer
    }

    /// Construct a DNS error response.
    fn mk_err_response<Target: Composer + Default>(
        &self,
        request: &Request<Vec<u8>>,
        rcode: Rcode,
    ) -> AnswerBuilder<StreamTarget<Target>> {
        let builder = mk_builder_for_target();
        builder.start_answer(request.message(), rcode).unwrap()
    }
}

//--- Service

impl<F> Service<Vec<u8>> for AgentService<F>
where
    F: Fn(u16, u16, RelativeName<Vec<u8>>),
{
    type Target = Vec<u8>;
    type Future = Ready<Result<CallResult<Self::Target>, ServiceError>>;

    fn call(
        &self,
        request: Request<Vec<u8>>,
    ) -> Result<Transaction<Self::Target, Self::Future>, ServiceError> {
        let additional = self.process_request(&request);
        let item = ready(Ok(CallResult::new(additional)));
        let txn = Transaction::single(item);
        Ok(txn)
    }
}
