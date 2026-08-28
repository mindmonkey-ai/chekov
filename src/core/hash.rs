//! Hand-rolled SHA-256 (FIPS 180-4) — house style: ~60 pure lines instead of
//! a `sha2` dependency for two hashing sites (machine identity, prompt-set
//! hash). Verified against the NIST test vectors below.

use std::fmt::Write;

/// FIPS 180-4 round constants: fractional parts of the cube roots of the
/// first 64 primes.
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// SHA-256 of `data`, as 64 lowercase hex characters.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    // Padding: 0x80, zeros to 56 mod 64, then the bit length as big-endian u64.
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        compress(&mut state, block);
    }
    let mut hex = String::with_capacity(64);
    for word in state {
        let _ = write!(hex, "{word:08x}");
    }
    hex
}

/// One 64-round compression over a 512-bit block. `reg` holds the eight
/// working registers FIPS names a..h, in that order.
fn compress(state: &mut [u32; 8], block: &[u8]) {
    let mut sched = [0u32; 64];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        sched[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for i in 16..64 {
        let s0 =
            sched[i - 15].rotate_right(7) ^ sched[i - 15].rotate_right(18) ^ (sched[i - 15] >> 3);
        let s1 =
            sched[i - 2].rotate_right(17) ^ sched[i - 2].rotate_right(19) ^ (sched[i - 2] >> 10);
        sched[i] = sched[i - 16]
            .wrapping_add(s0)
            .wrapping_add(sched[i - 7])
            .wrapping_add(s1);
    }
    let mut reg = *state;
    for i in 0..64 {
        let [ra, rb, rc, re, rf, rg] = [reg[0], reg[1], reg[2], reg[4], reg[5], reg[6]];
        let big_s1 = re.rotate_right(6) ^ re.rotate_right(11) ^ re.rotate_right(25);
        let choose = (re & rf) ^ (!re & rg);
        let temp1 = reg[7]
            .wrapping_add(big_s1)
            .wrapping_add(choose)
            .wrapping_add(K[i])
            .wrapping_add(sched[i]);
        let big_s0 = ra.rotate_right(2) ^ ra.rotate_right(13) ^ ra.rotate_right(22);
        let majority = (ra & rb) ^ (ra & rc) ^ (rb & rc);
        let temp2 = big_s0.wrapping_add(majority);
        reg.copy_within(0..7, 1);
        reg[4] = reg[4].wrapping_add(temp1); // e = d + temp1 (d shifted into slot 4)
        reg[0] = temp1.wrapping_add(temp2);
    }
    for (slot, val) in state.iter_mut().zip(reg) {
        *slot = slot.wrapping_add(val);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nist_vectors() {
        assert_eq!(
            super::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 56 bytes — crosses the padding boundary into a second block.
        assert_eq!(
            super::sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
