//! A refusal recognized only from result text, with no `rate_limit_event`
//! sample: hold the provider for a short cool-down anyway, or the refund
//! leaves nothing to stop the directive relaunching at once, indefinitely
//! (docs/REVIEW-3.md #1.1.4).

use super::Outcome;

pub fn hold_text_only(out: &mut Outcome) {
    out.rate_limited = true;
    if out.rate_limits.five_hour.is_none() {
        out.rate_limits.five_hour = Some((1.0, crate::unix_now() + 300));
    }
}
