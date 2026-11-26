//! URLs and outbound text come from the model: validation and the secret
//! scanner must hold up against anything.

use gardenwall_egress::scan::find_secrets;
use gardenwall_egress::target::validate;

#[test]
fn url_validation_and_secret_scan_survive_mutation() {
    gardenwall_fuzzkit::run_text(
        "egress",
        &[
            "https://user:pw@[::ffff:127.0.0.1]:8443/a/../b?q=%2e%2e#frag",
            "http://169.254.169.254/latest/meta-data/",
            "https://xn--80ak6aa92e.com/\u{202e}path AKIAABCDEFGHIJKLMNOP ghp_0123456789abcdefABCDEF0123456789abcd",
        ],
        5000,
        |t| {
            let _ = validate(t, &[]);
            let _ = find_secrets(t);
        },
    );
}
