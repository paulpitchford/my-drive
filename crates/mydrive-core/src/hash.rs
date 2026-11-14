//! quickXorHash, the only content hash available on every OneDrive account type (ADR-0011).
//!
//! The implementation is written from Microsoft's description and C# reference sample,
//! "QuickXOR Hash Sample" in the OneDrive developer documentation
//! (<https://learn.microsoft.com/en-us/onedrive/developer/code-snippets/quickxorhash>,
//! source `docs/code-snippets/quickxorhash.md` in `OneDrive/onedrive-api-docs` at commit
//! `8cdff2e5cbd362cf2bb3de9d8f102386af7abddd`). No code is copied from it.
//!
//! The test module contains test vectors (data, not code) taken from rclone; see the
//! provenance comment there and the `NOTICE` file (ADR-0002).
//!
//! # Algorithm
//!
//! The state is a 160-bit vector, stored as 20 bytes where bit `k` is bit `k % 8` of byte
//! `k / 8`. Input byte number `i` (counting from 0 over the whole stream) is XORed into the
//! state rotated left by `11 * i` bits, wrapping at 160 bits. At the end the total input
//! length, as a 64-bit little-endian integer, is XORed into the last 8 bytes of the state.

use std::fmt;
use std::io::{self, Read};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Length of a quickXorHash digest in bytes.
pub const DIGEST_LEN: usize = 20;

/// Width of the state in bits.
const WIDTH_BITS: usize = DIGEST_LEN * 8;

/// Rotation applied per input byte, in bits.
const SHIFT_BITS: usize = 11;

/// Buffer size used by [`hash_reader`].
const READ_BUF_LEN: usize = 64 * 1024;

/// A quickXorHash digest: 20 bytes, compared byte for byte.
///
/// Graph reports it base64-encoded in `file.hashes.quickXorHash`; use
/// [`QuickXorHash::from_base64`] and [`QuickXorHash::to_base64`] to convert.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuickXorHash([u8; DIGEST_LEN]);

impl QuickXorHash {
    /// Wraps raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    /// Parses the base64 form Graph returns in `file.hashes.quickXorHash`.
    ///
    /// Only canonical standard base64 with padding is accepted (28 characters for a
    /// 20-byte digest). Anything else is rejected rather than guessed at, because a wrongly
    /// parsed hash could make different content look identical.
    ///
    /// # Errors
    ///
    /// [`QuickXorHashParseError::InvalidBase64`] if `encoded` is not valid base64, and
    /// [`QuickXorHashParseError::WrongLength`] if it decodes to anything other than 20 bytes.
    pub fn from_base64(encoded: &str) -> Result<Self, QuickXorHashParseError> {
        let decoded = STANDARD
            .decode(encoded)
            .map_err(|source| QuickXorHashParseError::InvalidBase64 { source })?;
        let bytes: [u8; DIGEST_LEN] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| QuickXorHashParseError::WrongLength { len: decoded.len() })?;
        Ok(Self(bytes))
    }

    /// Encodes the digest in the base64 form Graph uses.
    #[must_use]
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

impl fmt::Debug for QuickXorHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QuickXorHash({})", self.to_base64())
    }
}

/// Why a base64 quickXorHash could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum QuickXorHashParseError {
    /// The text is not canonical standard base64.
    #[error("cannot parse quickXorHash: not valid base64: {source}")]
    InvalidBase64 {
        /// The decoder's error.
        #[source]
        source: base64::DecodeError,
    },
    /// The text is valid base64 but does not decode to a 20-byte digest.
    #[error("cannot parse quickXorHash: decoded to {len} bytes, expected {DIGEST_LEN}")]
    WrongLength {
        /// Number of bytes the text decoded to.
        len: usize,
    },
}

/// Streaming quickXorHash computation.
///
/// Call [`update`](Self::update) any number of times with consecutive pieces of the
/// content, then [`finalize`](Self::finalize). The digest does not depend on how the
/// content is split.
#[derive(Clone, Debug, Default)]
pub struct QuickXorHasher {
    state: [u8; DIGEST_LEN],
    /// Bit position in `state` where the next input byte starts, in `0..WIDTH_BITS`.
    shift: usize,
    /// Total bytes fed so far. Wraps rather than panics; the reference uses a 64-bit
    /// counter and no real file reaches 2^64 bytes.
    len: u64,
}

impl QuickXorHasher {
    /// A hasher that has seen no input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next piece of content.
    pub fn update(&mut self, data: &[u8]) {
        let mut shift = self.shift;
        for &byte in data {
            let index = shift / 8;
            let offset = shift % 8;
            // Widen so the byte can straddle two state bytes, wrapping from the last state
            // byte to the first (the 160-bit rotation).
            let wide = u16::from(byte) << offset;
            let [low, high] = wide.to_le_bytes();
            self.state[index] ^= low;
            self.state[(index + 1) % DIGEST_LEN] ^= high;
            shift = (shift + SHIFT_BITS) % WIDTH_BITS;
        }
        self.shift = shift;
        // usize is at most 64 bits on every supported target.
        self.len = self.len.wrapping_add(data.len() as u64);
    }

    /// Completes the computation and returns the digest.
    #[must_use]
    pub fn finalize(self) -> QuickXorHash {
        let mut out = self.state;
        let len_bytes = self.len.to_le_bytes();
        for (slot, len_byte) in out[DIGEST_LEN - len_bytes.len()..]
            .iter_mut()
            .zip(len_bytes)
        {
            *slot ^= len_byte;
        }
        QuickXorHash(out)
    }
}

/// Hashes everything `reader` yields until end of input.
///
/// Reads interrupted by a signal are retried.
///
/// # Errors
///
/// Returns the first other I/O error from `reader`; the caller knows which item was being
/// read and should add that context.
pub fn hash_reader<R: Read>(mut reader: R) -> io::Result<QuickXorHash> {
    let mut hasher = QuickXorHasher::new();
    let mut buf = vec![0u8; READ_BUF_LEN];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(hasher.finalize()),
            Ok(n) => {
                // A reader claiming more than the buffer holds is broken; hashing anything
                // would give a digest for content that was never read.
                let chunk = buf.get(..n).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "hashing content: reader reported more bytes than the buffer holds",
                    )
                })?;
                hasher.update(chunk);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn hash(data: &[u8]) -> QuickXorHash {
        let mut h = QuickXorHasher::new();
        h.update(data);
        h.finalize()
    }

    fn decode(s: &str) -> Vec<u8> {
        let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        STANDARD.decode(cleaned).unwrap()
    }

    // Test vectors (input and expected digest, both base64) copied as data from rclone,
    // https://github.com/rclone/rclone, file backend/onedrive/quickxorhash/quickxorhash_test.go
    // at commit b0ef5f6a6eca641a6bfb55bc05ef1595f4105d07, licence MIT (COPYING at the same
    // commit, "Copyright (C) 2012 by Nick Craig-Wood"). Retrieved 2026-11-14. Recorded in NOTICE.
    //
    // As an independent check, all 70 vectors were also reproduced on 2026-11-14 by
    // running Microsoft's C# reference sample (see the module header for its source)
    // unmodified on .NET 10; it matched every one.
    const RCLONE_VECTORS: &[(usize, &str, &str)] = &[
        (0, "", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        (1, "Sg==", "SgAAAAAAAAAAAAAAAQAAAAAAAAA="),
        (2, "tbQ=", "taAFAAAAAAAAAAAAAgAAAAAAAAA="),
        (3, "0pZP", "0rDEEwAAAAAAAAAAAwAAAAAAAAA="),
        (4, "jRRDVA==", "jaDAEKgAAAAAAAAABAAAAAAAAAA="),
        (5, "eAV52qE=", "eChAHrQRCgAAAAAABQAAAAAAAAA="),
        (6, "luBZlaT6", "lgBHFipBCn0AAAAABgAAAAAAAAA="),
        (7, "qaApEj66lw==", "qQBFCiTgA11cAgAABwAAAAAAAAA="),
        (8, "/aNzzCFPS/A=", "/RjFHJgRgicsAR4ACAAAAAAAAAA="),
        (9, "n6Neh7p6fFgm", "nxiFFw6hCz3wAQsmCQAAAAAAAAA="),
        (10, "J9iPGCbfZSTNyw==", "J8DGIzBggm+UgQTNUgYAAAAAAAA="),
        (11, "i+UZyUGJKh+ISbk=", "iyhHBpIRhESo4AOIQ0IuAAAAAAA="),
        (12, "h490d57Pqz5q2rtT", "h3gEHe7giWeswgdq3MYupgAAAAA="),
        (13, "vPgoDjOfO6fm71RxLw==", "vMAHChwwg0/s4BTmdQcV4vACAAA="),
        (14, "XoJ1AsoR4fDYJrDqYs4=", "XhBEHQSgjAiEAx7YPgEs1CEGZwA="),
        (15, "gQaybEqS/4UlDc8e4IJm", "gDCALNigBEn8oxAlZ8AzPAAOQZg="),
        (
            16,
            "2fuxhBJXtpWFe8dOfdGeHw==",
            "O9tHLAghgSvYohKFyMMxnNCHaHg=",
        ),
        (
            17,
            "XBV6YKU9V7yMakZnFIxIkuU=",
            "HbplHsBQih5cgReMQYMRzkABRiA=",
        ),
        (
            18,
            "XJZSOiNO2bmfKnTKD7fztcQX",
            "/6ZArHQwAidkIxefQgEdlPGAW8w=",
        ),
        (
            19,
            "g8VtAh+2Kf4k0kY5tzji2i2zmA==",
            "wDNrgwHWAVukwB8kg4YRcnALHIg=",
        ),
        (
            20,
            "T6LYJIfDh81JrAK309H2JMJTXis=",
            "zBTHrspn3mEcohlJdIUAbjGNaNg=",
        ),
        (
            21,
            "DWAAX5/CIfrmErgZa8ot6ZraeSbu",
            "LR2Z0PjuRYGKQB/mhQAuMrAGZbQ=",
        ),
        (
            22,
            "N9abi3qy/mC1THZuVLHPpx7SgwtLOA==",
            "1KTYttCBEen8Hwy1doId3ECFWDw=",
        ),
        (
            23,
            "LlUe7wHerLqEtbSZLZgZa9u0m7hbiFs=",
            "TqVZpxs3cN61BnuFvwUtMtECTGQ=",
        ),
        (
            24,
            "bU2j/0XYdgfPFD4691jV0AOUEUPR4Z5E",
            "bnLBiLpVgnxVkXhNsIAPdHAPLFQ=",
        ),
        (
            25,
            "lScPwPsyUsH2T1Qsr31wXtP55Wqbe47Uyg==",
            "VDMSy8eI26nBHCB0e8gVWPCKPsA=",
        ),
        (
            26,
            "rJaKh1dLR1k+4hynliTZMGf8Nd4qKKoZiAM=",
            "r7bjwkl8OYQeNaMcCY8fTmEJEmQ=",
        ),
        (
            27,
            "pPsT0CPmHrd3Frsnva1pB/z1ytARLeHEYRCo",
            "Rdg7rCcDomL59pL0s6GuTvqLVqQ=",
        ),
        (
            28,
            "wSRChaqmrsnMrfB2yqI43eRWbro+f9kBvh+01w==",
            "YTtloIi6frI7HX3vdLvE7I2iUOA=",
        ),
        (
            29,
            "apL67KMIRxQeE9k1/RuW09ppPjbF1WeQpTjSWtI=",
            "CIpedls+ZlSQ654fl+X26+Q7LVU=",
        ),
        (
            30,
            "53yx0/QgMTVb7OOzHRHbkS7ghyRc+sIXxi7XHKgT",
            "zfJtLGFgR9DB3Q64fAFIp+S5iOY=",
        ),
        (
            31,
            "PwXNnutoLLmxD8TTog52k8cQkukmT87TTnDipKLHQw==",
            "PTaGs7yV3FUyBy/SfU6xJRlCJlI=",
        ),
        (
            32,
            "NbYXsp5/K6mR+NmHwExjvWeWDJFnXTKWVlzYHoesp2E=",
            "wjuAuWDiq04qDt1R8hHWDDcwVoQ=",
        ),
        (
            33,
            "qQ70RB++JAR5ljNv3lJt1PpqETPsckopfonItu18Cr3E",
            "FkJaeg/0Z5+euShYlLpE2tJh+Lo=",
        ),
        (
            34,
            "RhzSatQTQ9/RFvpHyQa1WLdkr3nIk6MjJUma998YRtp44A==",
            "SPN2D29reImAqJezlqV2DLbi8tk=",
        ),
        (
            35,
            "DND1u1uZ5SqZVpRUk6NxSUdVo7IjjL9zs4A1evDNCDLcXWc=",
            "S6lBk2hxI2SWBfn7nbEl7D19UUs=",
        ),
        (
            36,
            "jEi62utFz69JMYHjg1iXy7oO6ZpZSLcVd2B+pjm6BGsv/CWi",
            "s0lYU9tr/bp9xsnrrjYgRS5EvV8=",
        ),
        (
            37,
            "hfS3DZZnhy0hv7nJdXLv/oJOtIgAuP9SInt/v8KeuO4/IvVh4A==",
            "CV+HQCdd2A/e/vdi12f2UU55GLA=",
        ),
        (
            38,
            "EkPQAC6ymuRrYjIXD/LT/4Vb+7aTjYVZOHzC8GPCEtYDP0+T3Nc=",
            "kE9H9sEmr3vHBYUiPbvsrcDgSEo=",
        ),
        (
            39,
            "vtBOGIENG7yQ/N7xNWPNIgy66Gk/I2Ur/ZhdFNUK9/1FCZuu/KeS",
            "+Fgp3HBimtCzUAyiinj3pkarYTk=",
        ),
        (
            40,
            "YnF4smoy9hox2jBlJ3VUa4qyCRhOZbWcmFGIiszTT4zAdYHsqJazyg==",
            "arkIn+ELddmE8N34J9ydyFKW+9w=",
        ),
        (
            41,
            "0n7nl3YJtipy6yeUbVPWtc2h45WbF9u8hTz5tNwj3dZZwfXWkk+GN3g=",
            "YJLNK7JR64j9aODWfqDvEe/u6NU=",
        ),
        (
            42,
            "FnIIPHayc1pHkY4Lh8+zhWwG8xk6Knk/D3cZU1/fOUmRAoJ6CeztvMOL",
            "22RPOylMtdk7xO/QEQiMli4ql0k=",
        ),
        (
            43,
            "J82VT7ND0Eg1MorSfJMUhn+qocF7PsUpdQAMrDiHJ2JcPZAHZ2nyuwjoKg==",
            "pOR5eYfwCLRJbJsidpc1rIJYwtM=",
        ),
        (
            44,
            "Zbu+78+e35ZIymV5KTDdub5McyI3FEO8fDxs62uWHQ9U3Oh3ZqgaZ30SnmQ=",
            "DbvbTkgNTgWRqRidA9r1jhtUjro=",
        ),
        (
            45,
            "lgybK3Da7LEeY5aeeNrqcdHvv6mD1W4cuQ3/rUj2C/CNcSI0cAMw6vtpVY3y",
            "700RQByn1lRQSSme9npQB/Ye+bY=",
        ),
        (
            46,
            "jStZgKHv4QyJLvF2bYbIUZi/FscHALfKHAssTXkrV1byVR9eACwW9DNZQRHQwg==",
            "uwN55He8xgE4g93dH9163xPew4U=",
        ),
        (
            47,
            "V1PSud3giF5WW72JB/bgtltsWtEB5V+a+wUALOJOGuqztzVXUZYrvoP3XV++gM0=",
            "U+3ZfUF/6mwOoHJcSHkQkckfTDA=",
        ),
        (
            48,
            "VXs4t4tfXGiWAL6dlhEMm0YQF0f2w9rzX0CvIVeuW56o6/ec2auMpKeU2VeteEK5",
            "sq24lSf7wXLH8eigHl07X+qPTps=",
        ),
        (
            49,
            "bLUn3jLH+HFUsG3ptWTHgNvtr3eEv9lfKBf0jm6uhpqhRwtbEQ7Ovj/hYQf42zfdtQ==",
            "uC8xrnopGiHebGuwgq607WRQyxQ=",
        ),
        (
            50,
            "4SVmjtXIL8BB8SfkbR5Cpaljm2jpyUfAhIBf65XmKxHlz9dy5XixgiE/q1lv+esZW/E=",
            "wxZ0rxkMQEnRNAp8ZgEZLT4RdLM=",
        ),
        (
            51,
            "pMljctlXeFUqbG3BppyiNbojQO3ygg6nZPeUZaQcVyJ+Clgiw3Q8ntLe8+02ZSfyCc39",
            "aZEPmNvOXnTt7z7wt+ewV7QGMlg=",
        ),
        (
            52,
            "C16uQlxsHxMWnV2gJhFPuJ2/guZ4N1YgmNvAwL1yrouGQtwieGx8WvZsmYRnX72JnbVtTw==",
            "QtlSNqXhVij64MMhKJ3EsDFB/z8=",
        ),
        (
            53,
            "7ZVDOywvrl3L0GyKjjcNg2CcTI81n2CeUbzdYWcZOSCEnA/xrNHpiK01HOcGh3BbxuS4S6g=",
            "4NznNJc4nmXeApfiCFTq/H5LbHw=",
        ),
        (
            54,
            "JXm2tTVqpYuuz2Cc+ZnPusUb8vccPGrzWK2oVwLLl/FjpFoxO9FxGlhnB08iu8Q/XQSdzHn+",
            "IwE5+2pKNcK366I2k2BzZYPibSI=",
        ),
        (
            55,
            "TiiU1mxzYBSGZuE+TX0l9USWBilQ7dEml5lLrzNPh75xmhjIK8SGqVAkvIMgAmcMB+raXdMPZg==",
            "yECGHtgR128ScP4XlvF96eLbIBE=",
        ),
        (
            56,
            "zz+Q4zi6wh0fCJUFU9yUOqEVxlIA93gybXHOtXIPwQQ44pW4fyh6BRgc1bOneRuSWp85hwlTJl8=",
            "+3Ef4D6yuoC8J+rbFqU1cegverE=",
        ),
        (
            57,
            "sa6SHK9z/G505bysK5KgRO2z2cTksDkLoFc7sv0tWBmf2G2mCiozf2Ce6EIO+W1fRsrrtn/eeOAV",
            "xZg1CwMNAjN0AIXw2yh4+1N3oos=",
        ),
        (
            58,
            "0qx0xdyTHhnKJ22IeTlAjRpWw6y2sOOWFP75XJ7cleGJQiV2kyrmQOST4DGHIL0qqA7sMOdzKyTV
iw==",
            "bS0tRYPkP1Gfc+ZsBm9PMzPunG8=",
        ),
        (
            59,
            "QuzaF0+5ooig6OLEWeibZUENl8EaiXAQvK9UjBEauMeuFFDCtNcGs25BDtJGGbX90gH4VZvCCDNC
q4s=",
            "rggokuJq1OGNOfB6aDp2g4rdPgw=",
        ),
        (
            60,
            "+wg2x23GZQmMLkdv9MeAdettIWDmyK6Wr+ba23XD+Pvvq1lIMn9QIQT4Z7QHJE3iC/ZMFgaId9VA
yY3d",
            "ahQbTmOdiKUNdhYRHgv5/Ky+Y6k=",
        ),
        (
            61,
            "y0ydRgreRQwP95vpNP92ioI+7wFiyldHRbr1SfoPNdbKGFA0lBREaBEGNhf9yixmfE+Azo2AuROx
b7Yc7g==",
            "cJKFc0dXfiN4hMg1lcMf5E4gqvo=",
        ),
        (
            62,
            "LxlVvGXSQlSubK8r0pGf9zf7s/3RHe75a2WlSXQf3gZFR/BtRnR7fCIcaG//CbGfodBFp06DBx/S
9hUV8Bk=",
            "NwuwhhRWX8QZ/vhWKWgQ1+rNomI=",
        ),
        (
            63,
            "L+LSB8kmGMnHaWVA5P/+qFnfQliXvgJW7d2JGAgT6+koi5NQujFW1bwQVoXrBVyob/gBxGizUoJM
gid5gGNo",
            "ndX/KZBtFoeO3xKeo1ajO/Jy+rY=",
        ),
        (
            64,
            "Mb7EGva2rEE5fENDL85P+BsapHEEjv2/siVhKjvAQe02feExVOQSkfmuYzU/kTF1MaKjPmKF/w+c
bvwfdWL8aQ==",
            "n1anP5NfvD4XDYWIeRPW3ZkPv1Y=",
        ),
        (
            111,
            "jyibxJSzO6ZiZ0O1qe3tG/bvIAYssvukh9suIT5wEy1JBINVgPiqdsTW0cOpP0aUfP7mgqLfADkz
I/m/GgCuVhr8oFLrOCoTx1/psBOWwhltCbhUx51Icm9aH8tY4Z3ccU+6BKpYQkLCy0B/A9Zc",
            "hZfLIilSITC6N3e3tQ/iSgEzkto=",
        ),
        (
            128,
            "ikwCorI7PKWz17EI50jZCGbV9JU2E8bXVfxNMg5zdmqSZ2NlsQPp0kqYIPjzwTg1MBtfWPg53k0h
0P2naJNEVgrqpoHTfV2b3pJ4m0zYPTJmUX4Bg/lOxcnCxAYKU29Y5F0U8Quz7ZXFBEweftXxJ7RS
4r6N7BzJrPsLhY7hgck=",
            "imAoFvCWlDn4yVw3/oq1PDbbm6U=",
        ),
        (
            222,
            "PfxMcUd0vIW6VbHG/uj/Y0W6qEoKmyBD0nYebEKazKaKG+UaDqBEcmQjbfQeVnVLuodMoPp7P7TR
1htX5n2VnkHh22xDyoJ8C/ZQKiSNqQfXvh83judf4RVr9exJCud8Uvgip6aVZTaPrJHVjQhMCp/d
EnGvqg0oN5OVkM2qqAXvA0teKUDhgNM71sDBVBCGXxNOR2bpbD1iM4dnuT0ey4L+loXEHTL0fqMe
UcEi2asgImnlNakwenDzz0x57aBwyq3AspCFGB1ncX4yYCr/OaCcS5OKi/00WH+wNQU3",
            "QX/YEpG0gDsmhEpCdWhsxDzsfVE=",
        ),
        (
            256,
            "qwGf2ESubE5jOUHHyc94ORczFYYbc2OmEzo+hBIyzJiNwAzC8PvJqtTzwkWkSslgHFGWQZR2BV5+
uYTrYT7HVwRM40vqfj0dBgeDENyTenIOL1LHkjtDKoXEnQ0mXAHoJ8PjbNC93zi5TovVRXTNzfGE
s5dpWVqxUzb5lc7dwkyvOluBw482mQ4xrzYyIY1t+//OrNi1ObGXuUw2jBQOFfJVj2Y6BOyYmfB1
y36eBxi3zxeG5d5NYjm2GSh6e08QMAwu3zrINcqIzLOuNIiGXBtl7DjKt7b5wqi4oFiRpZsCyx2s
mhSrdrtK/CkdU6nDN+34vSR/M8rZpWQdBE7a8g==",
            "WYT9JY3JIo/pEBp+tIM6Gt2nyTM=",
        ),
        (
            333,
            "w0LGhqU1WXFbdavqDE4kAjEzWLGGzmTNikzqnsiXHx2KRReKVTxkv27u3UcEz9+lbMvYl4xFf2Z4
aE1xRBBNd1Ke5C0zToSaYw5o4B/7X99nKK2/XaUX1byLow2aju2XJl2OpKpJg+tSJ2fmjIJTkfuY
Uz574dFX6/VXxSxwGH/xQEAKS5TCsBK3CwnuG1p5SAsQq3gGVozDWyjEBcWDMdy8/AIFrj/y03Lf
c/RNRCQTAfZbnf2QwV7sluw4fH3XJr07UoD0YqN+7XZzidtrwqMY26fpLZnyZjnBEt1FAZWO7RnK
G5asg8xRk9YaDdedXdQSJAOy6bWEWlABj+tVAigBxavaluUH8LOj+yfCFldJjNLdi90fVHkUD/m4
Mr5OtmupNMXPwuG3EQlqWUVpQoYpUYKLsk7a5Mvg6UFkiH596y5IbJEVCI1Kb3D1",
            "e3+wo77iKcILiZegnzyUNcjCdoQ=",
        ),
    ];

    /// Byte `i` of the generated test inputs used by [`MICROSOFT_REFERENCE_VECTORS`].
    fn pattern_byte(i: usize) -> u8 {
        u8::try_from((i * 31 + 7) % 251).unwrap()
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(pattern_byte).collect()
    }

    // Expected digests for inputs generated by `pattern(len)`, i.e. byte i is
    // (i * 31 + 7) mod 251. rclone has no vector over 333 bytes and none at the 160-byte
    // rotation period, so these were computed on 2026-11-14 by running Microsoft's C#
    // reference sample (source in the module header, transcribed unmodified, run on .NET 10)
    // once on the whole input and once in 4093-byte chunks; both gave the values below.
    // Before use, the same transcription was checked against all 70 rclone vectors above
    // and matched every one. These values were not produced by this crate's code.
    const MICROSOFT_REFERENCE_VECTORS: &[(usize, &str)] = &[
        (1, "BwAAAAAAAAAAAAAAAQAAAAAAAAA="),
        (20, "pStAEQk/tFEEAxwEDIEQwgCIT/g="),
        (159, "Oz47J5QjmNjYfAzJ0o+5/A9YB4I="),
        (160, "Oz47J5QjmNjYfAzJ7Y+5/A9Y55Y="),
        (161, "/T47J5QjmNjYfAzJ7I+5/A9Y55Y="),
        (320, "7OlrnWOAC/NzDe55I2QS13cgAjM="),
        (1_048_576, "4wGVo5hvyExHBXKXxBSb4Tk8CX0="),
        (1_048_577, "4wGVo5hvyExHBXKXqRSb4Tk8CX0="),
        (3_145_741, "OoEslycI/wrk85FDfU0KoNs5ZNU="),
    ];

    /// AC1: the empty input hashes to the reference value (all zero bytes).
    #[test]
    fn empty_input_matches_reference() {
        let got = QuickXorHasher::new().finalize();
        assert_eq!(got.to_base64(), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
        assert_eq!(got, QuickXorHash::from_bytes([0; DIGEST_LEN]));
    }

    /// AC2: rclone's vectors, 0 to 333 bytes, covering inputs shorter than, equal to and
    /// longer than the 20-byte state.
    #[test]
    fn rclone_reference_vectors() {
        assert_eq!(RCLONE_VECTORS.len(), 70);
        for &(len, input, expected) in RCLONE_VECTORS {
            let input = decode(input);
            assert_eq!(
                input.len(),
                len,
                "vector of size {len} decoded to wrong length"
            );
            assert_eq!(hash(&input).to_base64(), expected, "vector of size {len}");
        }
    }

    /// AC2: vectors from Microsoft's reference, around the 160-byte rotation period and
    /// over 1 MiB.
    #[test]
    fn microsoft_reference_vectors_including_over_one_mib() {
        assert!(
            MICROSOFT_REFERENCE_VECTORS
                .iter()
                .any(|&(len, _)| len > 1024 * 1024)
        );
        for &(len, expected) in MICROSOFT_REFERENCE_VECTORS {
            assert_eq!(
                hash(&pattern(len)).to_base64(),
                expected,
                "pattern of size {len}"
            );
        }
    }

    /// AC2 and the reader convenience: `hash_reader` gives the reference digest for a
    /// multi-MiB input, which spans many internal read buffers.
    #[test]
    fn hash_reader_matches_reference() {
        let &(len, expected) = MICROSOFT_REFERENCE_VECTORS.last().unwrap();
        let got = hash_reader(io::Cursor::new(pattern(len))).unwrap();
        assert_eq!(got.to_base64(), expected);
    }

    /// A reader that returns at most `max` bytes per call and is interrupted every other
    /// call, to exercise short reads and the retry on `Interrupted`.
    struct AwkwardReader {
        data: Vec<u8>,
        pos: usize,
        max: usize,
        interrupt_next: bool,
    }

    impl Read for AwkwardReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.interrupt_next = !self.interrupt_next;
            if self.interrupt_next {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let n = buf.len().min(self.max).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn hash_reader_retries_interrupted_and_handles_short_reads() {
        let &(len, expected) = &MICROSOFT_REFERENCE_VECTORS[5];
        let reader = AwkwardReader {
            data: pattern(len),
            pos: 0,
            max: 7,
            interrupt_next: false,
        };
        assert_eq!(hash_reader(reader).unwrap().to_base64(), expected);
    }

    #[test]
    fn hash_reader_propagates_other_errors() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("disk on fire"))
            }
        }
        let err = hash_reader(Failing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    /// AC3: every rclone vector gives the same digest under fixed chunk sizes, matching
    /// rclone's own block-wise test.
    #[test]
    fn rclone_vectors_fixed_chunkings() {
        for chunk in [1, 2, 4, 7, 8, 16, 20, 32, 64, 128, 159, 160, 161, 256, 512] {
            for &(len, input, expected) in RCLONE_VECTORS {
                let input = decode(input);
                let mut h = QuickXorHasher::new();
                for piece in input.chunks(chunk) {
                    h.update(piece);
                }
                assert_eq!(
                    h.finalize().to_base64(),
                    expected,
                    "size {len} chunk {chunk}"
                );
            }
        }
    }

    /// AC4: encoding and decoding round-trip.
    #[test]
    fn base64_round_trip_of_reference_digests() {
        for &(_, _, expected) in RCLONE_VECTORS {
            let parsed = QuickXorHash::from_base64(expected).unwrap();
            assert_eq!(parsed.to_base64(), expected);
        }
    }

    /// AC4: malformed base64 is rejected with `InvalidBase64`.
    #[test]
    fn malformed_base64_is_rejected() {
        let cases = [
            "not base64!",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAA",    // missing padding
            "AAAAAAAAAAAAAAAAAAAAAAAAAAA==",  // excess padding
            "AAAAAAAAAAAAAAAAAAAAAAAAAAA=\n", // trailing whitespace
            " AAAAAAAAAAAAAAAAAAAAAAAAAAA=",  // leading whitespace
            "AAAAAAAAAAAAAAAAAAAAAAAAAAB=",   // non-zero trailing bits (non-canonical)
            "AAAAAAAAAAAAAAAAAAAAAAAAAA-_",   // URL-safe alphabet
        ];
        for case in cases {
            assert!(
                matches!(
                    QuickXorHash::from_base64(case),
                    Err(QuickXorHashParseError::InvalidBase64 { .. })
                ),
                "{case:?} should be rejected as invalid base64"
            );
        }
    }

    /// AC4: valid base64 of the wrong length is rejected with `WrongLength`.
    #[test]
    fn wrong_length_base64_is_rejected() {
        for len in [0usize, 1, 19, 21, 32] {
            let encoded = STANDARD.encode(vec![0xA5; len]);
            match QuickXorHash::from_base64(&encoded) {
                Err(QuickXorHashParseError::WrongLength { len: got }) => assert_eq!(got, len),
                other => panic!("{len} bytes: expected WrongLength, got {other:?}"),
            }
        }
    }

    #[test]
    fn debug_shows_base64() {
        let h = QuickXorHash::from_base64("SgAAAAAAAAAAAAAAAQAAAAAAAAA=").unwrap();
        assert_eq!(
            format!("{h:?}"),
            "QuickXorHash(SgAAAAAAAAAAAAAAAQAAAAAAAAA=)"
        );
    }

    proptest! {
        /// AC3: any split of the same bytes into consecutive pieces gives the same digest.
        #[test]
        fn any_chunking_gives_same_digest(
            data in proptest::collection::vec(any::<u8>(), 0..2048),
            cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..16),
        ) {
            let mut points: Vec<usize> = cuts.iter().map(|c| c.index(data.len() + 1)).collect();
            points.push(0);
            points.push(data.len());
            points.sort_unstable();
            let mut h = QuickXorHasher::new();
            for w in points.windows(2) {
                h.update(&data[w[0]..w[1]]);
            }
            prop_assert_eq!(h.finalize(), hash(&data));
        }

        /// AC4: every digest survives a base64 round trip.
        #[test]
        fn base64_round_trip(bytes in any::<[u8; DIGEST_LEN]>()) {
            let digest = QuickXorHash::from_bytes(bytes);
            prop_assert_eq!(QuickXorHash::from_base64(&digest.to_base64()).unwrap(), digest);
        }
    }
}
