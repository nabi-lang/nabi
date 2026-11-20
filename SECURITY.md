# Security policy

This is the toolchain's policy; the policy for every nabi-lang repository is
at https://github.com/nabi-lang/.github/blob/main/SECURITY.md.

## Reporting a vulnerability

Please do not report security issues in public issues or pull requests.

Use GitHub's private vulnerability reporting: on this repository, open the
**Security** tab and choose **Report a vulnerability**. You can also email security@nabi-lang.org.

We will acknowledge the report, work with you on a fix, and credit you in the
advisory unless you prefer otherwise.

## Cryptography and TLS

`core::crypto` and `std::net::tls` are written in Nabi from the
published standards. They are tested against NIST CAVP and Wycheproof vectors
and checked for secret-dependent timing, but they are experimental and have
not had an independent security audit. Do not rely on them to protect real
secrets yet. Reports about them are welcome through the channels above.

## Supported versions

Nabi is pre-1.0. Fixes and security fixes land on the latest 0.x series only;
move to it with `nabi toolchain update`.
