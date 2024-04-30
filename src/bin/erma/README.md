# Erma - Error Report Monitoring Agent

Erma is an implementation of an [RFC 9567](https://datatracker.ietf.org/doc/html/rfc9567#name-monitoring-agent-specificat) monitoring agent.

## Building

```
$ cargo build --release --bin erma
```

## Usage

Erma is intended to sit in between two other actors: an authoritative DNS server and an error report propagator.

The authoritative DNS server must respond to EDNS capable clients with an EDNS0 Report-Channel option in its responses to resolvers. The option value should point to where Erma is running.

On applicable error RFC 9567 aware resolvers will then attempt to query Erma using a specific form of TXT query.

On receipt of a valid query Erma will output a CSV version of the received report to its STDOUT in the form: `<report decimal qtype>,<report decimal edns error code>,<report qname>`.

The STDOUT of Erma should be piped into a user supplied report propagation tool which will forward the report to the correct destination where monitoring operators/systems will detect and handle it.

## Testing

A simple local test without an authoritative server or resolver or report propagator can be performed just using Erma and a DNS client tool such as `idns` or `dig`.

1. In terminal **1** run Erma:

    _(By default Erma will bind to port 53 on all interfacess which usually requires root access so we override that for a quick test)_
```
$ target/release/erma --addr 127.0.0.1 --port 8053
```

2. In termnal **2** submit a query to Erma, e.g. using `idns` or `dig`:

    _(The query shown here is the [RFC 9567 example](https://datatracker.ietf.org/doc/html/rfc9567#name-example))_
```
$ idns query -s 127.0.0.1 -p 8053 _er.1.broken.test.7._er.a01.agent-domain.example. TXT
```

3. In terminal **1** on the STDOUT of Erma you should see a CSV formatted version of the received error report:

```
1,7,broken.test
```

4. In terminal **2** the query response should be `NOERROR`:

```
;; ->>HEADER<<- opcode: QUERY, rcode: NOERROR, id: 51847
;; flags: QR RD; QUERY: 1, ANSWER: 1, AUTHORITY: 0, ADDITIONAL: 0
;; QUESTION SECTION:; _er.1.broken.test.7._er.a01.agent-domain.example. TXT     IN

;; ANSWER SECTION:
_er.1.broken.test.7._er.a01.agent-domain.example. 86400 IN TXT "Report received"

;; Query time: 0 msec
;; SERVER: 127.0.0.1#8053 (UDP)
;; WHEN: Tue Apr 30 11:54:11 +02:00 2024
;; MSG SIZE  rcvd: 142
```

