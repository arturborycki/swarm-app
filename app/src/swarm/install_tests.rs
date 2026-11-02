use super::*;

/// Swarm 0.1.1's DMG, as signed for the release feed.
fn release_0_1_1() -> Signed {
    Signed {
        sha256: "f580353a8d48b6529b0620ad2e93e0374e3792eff5ce6965d874f23604aeeec7".into(),
        signature: "KR3B3OM37+SK1Lk4+edQu4Xsi3tYmT6WgHObjhaDcSprTojf/g5GLor+Bkg8/JPw2cniiGYdUD7fLZHnM0O5Dw==".into(),
    }
}

#[test]
fn release_signature_verifies() {
    verify_signature("0.1.1", &release_0_1_1()).unwrap();
}

#[test]
fn signature_is_for_one_file_and_one_version() {
    assert!(verify_signature("0.1.2", &release_0_1_1()).is_err());
    let other_file = Signed { sha256: "0".repeat(64), ..release_0_1_1() };
    assert!(verify_signature("0.1.1", &other_file).is_err());
    let garbage = Signed { signature: "not base64!".into(), ..release_0_1_1() };
    assert!(verify_signature("0.1.1", &garbage).is_err());
}
