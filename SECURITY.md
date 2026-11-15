# Security

NRESE is a hobby project with one maintainer, no security team and no bug bounty. Reports are still welcome, and
this page says what happens with them.

## Supported versions

| Version | Security fixes |
|---|---|
| 1.5.x, the latest release | yes |
| `main` | yes: the next release is made from it |
| earlier versions, 0.1.0 included | no |

A fix goes to `main` and into a new 1.5.x release. Nothing older is patched.

## Reporting a vulnerability

Please report privately, never in a public issue, pull request or discussion: on the repository's **Security**
tab, choose **Report a vulnerability** (GitHub's private vulnerability reporting,
<https://github.com/Krarilotus/OWL-RS/security/advisories/new>). There is no mail address for reports.

What helps:

- the version or commit, and how you run it: the server or the crates, the store mode
  (`store.mode`), the deployment posture and any setting that matters to the finding;
- what an attacker can do, and what they need for it (network access only, an account, a role);
- the steps to reproduce it, or a proof of concept, as small as you can make it;
- the data it needs, if any. Please send no real personal data: made-up data that shows the problem is enough.

## What to expect

- An acknowledgement within 14 days.
- Within 30 days, an assessment: whether it is a vulnerability, how serious it is, and what will be done.
- Then an update at least every 30 days until it is fixed or declined, with the reasons if it is declined.

One person handles reports in spare time, so these are the times to rely on, not a measure of urgency. A report
that sits past them may be sent again through the same form.

## Disclosure

Please keep the report private until a fix is released, or for 90 days from the report, whichever comes first. If a
fix needs longer, you will hear why, and a date is agreed with you. A fix is published as a GitHub security advisory
in this repository, with a CVE where one applies. You are credited there unless you would rather not be.

## Out of scope

- Limits the documentation already states, such as memory outside a query's reservation or an in-memory load larger
  than the memory the process may have ([limits and deviations](docs/limits.md)). A report that a stated limit does not hold as
  described is in scope.
- A weakness in a dependency that does not affect NRESE. Report it to that project. If it does affect NRESE, report it
  here too.
- The benchmark suites, scripts and CI workflows (`benches/`, `scripts/`, `.github/`): they are not part of what is
  released.
- Findings from an automated scanner without a shown impact.
