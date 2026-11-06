use semver::Version;
use std::{cmp::Ordering, sync::LazyLock};

pub static ALVR_VERSION: LazyLock<Version> =
    LazyLock::new(|| Version::parse(env!("CARGO_PKG_VERSION")).unwrap());

// KiraVR: This used to be std's DefaultHasher, whose algorithm Rust does not
// promise to keep. The hash travels between headset and PC (protocol ID,
// button and input path IDs), so a toolchain update on one side could have
// silently broken pairing or input mapping. This is the same algorithm
// DefaultHasher uses today (SipHash-1-3, zero keys, str hashed as its bytes
// plus 0xff), written out so it can never change. The values are identical,
// see the test below, so KiraVR stays compatible with ALVR builds of the same
// version.
pub fn hash_string(string: &str) -> u64 {
    let mut bytes = Vec::with_capacity(string.len() + 1);
    bytes.extend_from_slice(string.as_bytes());
    bytes.push(0xff);
    siphash13(0, 0, &bytes)
}

fn siphash13(k0: u64, k1: u64, message: &[u8]) -> u64 {
    let mut v = [
        k0 ^ 0x736f_6d65_7073_6575,
        k1 ^ 0x646f_7261_6e64_6f6d,
        k0 ^ 0x6c79_6765_6e65_7261,
        k1 ^ 0x7465_6462_7974_6573,
    ];

    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }

    let mut chunks = message.chunks_exact(8);
    for chunk in &mut chunks {
        let m = u64::from_le_bytes(chunk.try_into().unwrap());
        v[3] ^= m;
        round(&mut v);
        v[0] ^= m;
    }

    let mut last = (message.len() as u64 & 0xff) << 56;
    for (i, byte) in chunks.remainder().iter().enumerate() {
        last |= (*byte as u64) << (8 * i);
    }
    v[3] ^= last;
    round(&mut v);
    v[0] ^= last;

    v[2] ^= 0xff;
    for _ in 0..3 {
        round(&mut v);
    }

    v[0] ^ v[1] ^ v[2] ^ v[3]
}

pub fn is_nightly() -> bool {
    ALVR_VERSION.build.contains("nightly")
}

pub fn is_stable() -> bool {
    ALVR_VERSION.pre.is_empty() && !is_nightly()
}

// Semver compatible versions will produce the same protocol ID. Protocol IDs are not ordered
// As a convention, encode/decode the protocol ID bytes as little endian.
// Only major and pre-release are part of it.
pub fn protocol_id() -> String {
    if ALVR_VERSION.pre.is_empty() {
        ALVR_VERSION.major.to_string()
    } else {
        format!("{}-{}", ALVR_VERSION.major, ALVR_VERSION.pre)
    }
}

pub fn protocol_id_u64() -> u64 {
    hash_string(&protocol_id())
}

// KiraVR: Orders two protocol IDs ("21", "21-dev14") so a mismatch can say
// which side is older. A release sorts after its pre-releases, and the number
// at the end of a pre-release compares numerically ("dev9" < "dev14"), which
// plain semver string ordering gets wrong. Returns None if either is malformed.
pub fn compare_protocols(a: &str, b: &str) -> Option<Ordering> {
    fn parse(protocol: &str) -> Option<(u64, Option<(String, u64)>)> {
        let (major, pre) = match protocol.split_once('-') {
            Some((major, pre)) => (major, Some(pre)),
            None => (protocol, None),
        };
        let major = major.trim().parse().ok()?;
        let pre = pre.map(|pre| {
            let digits = pre.len() - pre.trim_end_matches(|c: char| c.is_ascii_digit()).len();
            let (name, number) = pre.split_at(pre.len() - digits);
            (name.to_owned(), number.parse().unwrap_or(0))
        });
        Some((major, pre))
    }

    let (a, b) = (parse(a)?, parse(b)?);
    Some(a.0.cmp(&b.0).then_with(|| match (&a.1, &b.1) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => a.0.cmp(&b.0).then(a.1.cmp(&b.1)),
    }))
}

// deprecated
pub fn is_version_compatible(other_version: &Version) -> bool {
    let protocol_string = if other_version.pre.is_empty() {
        other_version.major.to_string()
    } else {
        format!("{}-{}", other_version.major, other_version.pre)
    };

    protocol_id_u64() == hash_string(&protocol_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(deprecated)]
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };

    fn std_hash(string: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        string.hash(&mut hasher);
        hasher.finish()
    }

    // The fixed implementation must keep producing what DefaultHasher did, or
    // KiraVR would stop pairing with existing clients.
    #[test]
    fn matches_default_hasher() {
        for s in [
            "",
            "2",
            "21",
            "21-dev14",
            "1234567",
            "12345678",
            "123456789012345",
            "1234567890123456",
            "/user/hand/left/input/a/click",
            "/interaction_profiles/oculus/touch_controller",
            "ünïcödé ✓",
        ] {
            assert_eq!(hash_string(s), std_hash(s), "mismatch for {s:?}");
        }
    }

    // Values recorded from DefaultHasher on Rust 1.97. If a future toolchain
    // changes DefaultHasher, the test above fails but this one must not.
    #[test]
    fn known_values_never_change() {
        assert_eq!(hash_string("21-dev14"), 0xb513_cab3_af77_0970);
        assert_eq!(hash_string("21"), 0x14e4_a81b_2520_38c6);
        assert_eq!(
            hash_string("/user/hand/left/input/a/click"),
            0x0665_55b8_9580_8431
        );
    }

    #[test]
    fn protocol_ordering() {
        use Ordering::*;
        assert_eq!(compare_protocols("21-dev14", "21-dev14"), Some(Equal));
        assert_eq!(compare_protocols("21-dev9", "21-dev14"), Some(Less));
        assert_eq!(compare_protocols("21", "21-dev14"), Some(Greater));
        assert_eq!(compare_protocols("20", "21-dev1"), Some(Less));
        assert_eq!(compare_protocols("22-dev1", "21"), Some(Greater));
        assert_eq!(compare_protocols("21-alpha2", "21-beta1"), Some(Less));
        assert_eq!(compare_protocols("x", "21"), None);
    }
}
