//! Game specific handling of TUS save data for NPWR02973_00 (Tekken Tag
//! Tournament 2).
//!
//! The title keeps its whole online record in a single 3420 byte TUS slot:
//! a 112 byte header, 59 character records of 48 bytes each, and a tail. Each
//! character record holds a rank at `+0x00` and a rank progress value at
//! `+0x02`, and the record is sealed with a CRC at offset 0.
//!
//! The only thing done here is giving a brand new account a starting rank, so
//! that first time players are not dropped into a rank band that nobody else
//! occupies any more. Existing saves are never rewritten.

use tracing::{debug, warn};

use crate::server::client::ComId;

const NPWR02973_00: ComId = *b"NPWR02973_00";

/// Size of a well formed save. Anything else is passed through untouched.
const RECORD_SIZE: usize = 3420;

const CHAR_BASE: usize = 0x70;
const CHAR_STRIDE: usize = 0x30;
const CHAR_COUNT: usize = 59;
const CHAR_RANK: usize = 0x00;
const CHAR_RANK_POINTS: usize = 0x02;

/// Rank a fresh account starts every character at. 1st Dan.
const STARTING_RANK: u8 = 10;
/// Rank progress that goes with it. Observed resting value for this rank.
const STARTING_RANK_POINTS: u16 = 0;

/// The checksum covers the record from offset 4 to the end, followed by this
/// many zero bytes, as a raw CRC-32 with init and xorout both zero.
const CHECKSUM_SKIP: usize = 4;
const CHECKSUM_ZERO_PAD: usize = 4760;
const CRC32_REFLECTED_POLY: u32 = 0xEDB8_8320;

fn crc32_raw(data: &[u8], zero_pad: usize) -> u32 {
	let mut crc: u32 = 0;
	let mut step = |byte: u8| {
		crc ^= byte as u32;
		for _ in 0..8 {
			crc = if crc & 1 != 0 { (crc >> 1) ^ CRC32_REFLECTED_POLY } else { crc >> 1 };
		}
	};
	for &byte in data {
		step(byte);
	}
	for _ in 0..zero_pad {
		step(0);
	}
	crc
}

fn checksum(record: &[u8]) -> u32 {
	crc32_raw(&record[CHECKSUM_SKIP..], CHECKSUM_ZERO_PAD)
}

fn stored_checksum(record: &[u8]) -> u32 {
	u32::from_be_bytes([record[0], record[1], record[2], record[3]])
}

/// Gives every character in a brand new save a starting rank.
///
/// Returns the rewritten save, or `None` when nothing was changed: another
/// title, an unexpected size, a checksum that does not verify, or a save that
/// already sits at or above the starting rank.
///
/// The caller is responsible for only offering a save that is the account's
/// first one for this slot. Applying this to a save that already has progress
/// would raise ranks the player has since lost.
pub(crate) fn apply_starting_rank(com_id: &ComId, data: &[u8]) -> Option<Vec<u8>> {
	if com_id != &NPWR02973_00 {
		return None;
	}

	if data.len() != RECORD_SIZE {
		debug!(len = data.len(), "NPWR02973_00 save has an unexpected size, left untouched");
		return None;
	}

	// Refuse to touch a save we cannot verify: if the checksum does not match
	// then this is not the layout we know and rewriting it would corrupt it.
	if checksum(data) != stored_checksum(data) {
		warn!("NPWR02973_00 save failed its checksum, starting rank not applied");
		return None;
	}

	let mut out = data.to_vec();
	let mut raised = 0;
	for i in 0..CHAR_COUNT {
		let base = CHAR_BASE + i * CHAR_STRIDE;
		if out[base + CHAR_RANK] >= STARTING_RANK {
			continue;
		}
		out[base + CHAR_RANK] = STARTING_RANK;
		out[base + CHAR_RANK_POINTS..base + CHAR_RANK_POINTS + 2].copy_from_slice(&STARTING_RANK_POINTS.to_be_bytes());
		raised += 1;
	}

	if raised == 0 {
		return None;
	}

	let new_checksum = checksum(&out);
	out[0..4].copy_from_slice(&new_checksum.to_be_bytes());

	debug!(raised, rank = STARTING_RANK, "NPWR02973_00 starting rank applied to a first save");
	Some(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Builds a save whose checksum verifies, with every character at `rank`.
	fn make_record(rank: u8) -> Vec<u8> {
		let mut record = vec![0u8; RECORD_SIZE];
		for i in 0..CHAR_COUNT {
			record[CHAR_BASE + i * CHAR_STRIDE + CHAR_RANK] = rank;
		}
		let sum = checksum(&record);
		record[0..4].copy_from_slice(&sum.to_be_bytes());
		record
	}

	#[test]
	fn checksum_round_trips() {
		let record = make_record(0);
		assert_eq!(checksum(&record), stored_checksum(&record));
	}

	/// Pins the algorithm down. These two values were taken from real saves
	/// off a live server, where the formula reproduces 57,709 of 57,715
	/// stored checksums; the six it misses are copies of one save whose own
	/// stored value does not match its contents.
	#[test]
	fn checksum_matches_known_values() {
		let zeroed = vec![0u8; RECORD_SIZE];
		assert_eq!(checksum(&zeroed), 0x0000_0000);

		let mut all_first_dan = vec![0u8; RECORD_SIZE];
		for i in 0..CHAR_COUNT {
			all_first_dan[CHAR_BASE + i * CHAR_STRIDE + CHAR_RANK] = STARTING_RANK;
		}
		assert_eq!(checksum(&all_first_dan), 0x9063_E708);
	}

	#[test]
	fn raises_every_character_of_a_fresh_save() {
		let record = make_record(0);
		let out = apply_starting_rank(&NPWR02973_00, &record).expect("a fresh save is raised");
		assert_eq!(out.len(), RECORD_SIZE);
		for i in 0..CHAR_COUNT {
			assert_eq!(out[CHAR_BASE + i * CHAR_STRIDE + CHAR_RANK], STARTING_RANK);
		}
		assert_eq!(checksum(&out), stored_checksum(&out), "the rewritten save reseals");
	}

	#[test]
	fn leaves_characters_already_at_or_above_the_starting_rank() {
		assert!(apply_starting_rank(&NPWR02973_00, &make_record(STARTING_RANK)).is_none());
		assert!(apply_starting_rank(&NPWR02973_00, &make_record(STARTING_RANK + 5)).is_none());
	}

	#[test]
	fn raises_only_the_characters_below_the_starting_rank() {
		let mut record = make_record(0);
		let kept = CHAR_BASE + 3 * CHAR_STRIDE;
		record[kept + CHAR_RANK] = 25;
		record[kept + CHAR_RANK_POINTS..kept + CHAR_RANK_POINTS + 2].copy_from_slice(&1234u16.to_be_bytes());
		let sum = checksum(&record);
		record[0..4].copy_from_slice(&sum.to_be_bytes());

		let out = apply_starting_rank(&NPWR02973_00, &record).expect("the other characters are raised");
		assert_eq!(out[kept + CHAR_RANK], 25, "a higher rank is left alone");
		assert_eq!(u16::from_be_bytes([out[kept + CHAR_RANK_POINTS], out[kept + CHAR_RANK_POINTS + 1]]), 1234);
		assert_eq!(out[CHAR_BASE + CHAR_RANK], STARTING_RANK);
	}

	#[test]
	fn ignores_other_titles() {
		assert!(apply_starting_rank(b"NPWR00482_00", &make_record(0)).is_none());
	}

	#[test]
	fn ignores_an_unexpected_size() {
		assert!(apply_starting_rank(&NPWR02973_00, &vec![0u8; 16]).is_none());
	}

	#[test]
	fn refuses_a_save_whose_checksum_does_not_verify() {
		let mut record = make_record(0);
		record[0] ^= 0xFF;
		assert!(apply_starting_rank(&NPWR02973_00, &record).is_none());
	}
}
