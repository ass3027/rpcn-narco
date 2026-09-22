//! Game specific handling of TUS save data for NPWR02973_00 (Tekken Tag
//! Tournament 2).
//!
//! The title keeps its whole online record in a single 3420 byte TUS slot:
//! a 112 byte header, 59 character records of 48 bytes each, and a tail. Each
//! character record holds a rank at `+0x00` and a rank progress value at
//! `+0x02`, and the record is sealed with a CRC at offset 0.
//!
//! Two things are done here, both to the ranks of characters the player is not
//! currently climbing with.
//!
//! A brand new account starts every character at rank 0, and the band below 1st
//! Dan is no longer occupied, so such a player finds nobody to play. Give a
//! first save a starting rank instead.
//!
//! Separately, ranks come in tiers, and a player whose best character crosses
//! into a new tier has their remaining characters raised to the floor for that
//! tier. Without this, a strong player picking up a second character enters
//! matchmaking near the bottom, because the rank the client offers for
//! matchmaking is the higher rank of the pair it is fielding.
//!
//! Both only ever raise a rank, and only at the moment the account crosses a
//! tier. A character that is later demoted below the floor stays there, so a
//! character raised past the player's actual ability still finds its way back
//! down.

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

/// The account level rank, which is a high water mark rather than a current
/// standing: it is never seen to go down.
const ACCOUNT_RANK: usize = 0x18;

/// Rank a fresh account starts every character at. 1st Dan.
const STARTING_RANK: u8 = 10;
/// Rank progress that goes with it. Observed resting value for this rank.
const STARTING_RANK_POINTS: u16 = 0;

/// First rank of each tier. The floor for an account is the first rank of the
/// tier two below the one its best character has reached, which is the same as
/// eight ranks down wherever the tiers are four ranks apart.
const TIERS: [u8; 9] = [10, 13, 17, 21, 25, 29, 33, 38, 41];
/// How many tiers below the account's own tier the floor sits.
const TIER_DROP: usize = 2;

/// Rank progress written to a character that is raised to a given rank.
///
/// Below 1st Dan the ranks sit on a single absolute ladder of `200 * rank`,
/// which holds without exception across the whole corpus. 1st Dan resets to
/// zero. Above that each rank has its own range and no closed form, so these
/// are the tenth percentile of the observed distribution: a raised character
/// lands near the bottom of its new rank rather than one win from leaving it.
/// Ranks with too thin a sample are interpolated from their neighbours.
fn rank_points_for(rank: u8) -> u16 {
	const OBSERVED: [(u8, u16); 10] = [(11, 2531), (12, 2735), (13, 2300), (14, 3112), (15, 2907), (16, 2799), (17, 5993), (18, 1562), (19, 1964), (21, 2679)];

	if rank == 0 {
		return 0;
	}
	if rank < 10 {
		return 200 * rank as u16;
	}
	if rank == 10 {
		return STARTING_RANK_POINTS;
	}
	if let Some(&(_, points)) = OBSERVED.iter().find(|&&(r, _)| r == rank) {
		return points;
	}

	let below = OBSERVED.iter().filter(|&&(r, _)| r < rank).next_back();
	let above = OBSERVED.iter().find(|&&(r, _)| r > rank);
	match (below, above) {
		(Some(&(lo_rank, lo)), Some(&(hi_rank, hi))) => {
			let span = (hi_rank - lo_rank) as u32;
			let step = (rank - lo_rank) as u32;
			let interpolated = lo as u32 + (hi as u32 - lo as u32) * step / span;
			interpolated as u16
		}
		(Some(&(_, lo)), None) => lo,
		(None, Some(&(_, hi))) => hi,
		(None, None) => 0,
	}
}

/// The rank every character of this account is entitled to, given the best
/// rank it has reached.
fn floor_for(best_rank: u8) -> u8 {
	let tier = TIERS.iter().rposition(|&t| best_rank >= t);
	match tier {
		Some(index) if index >= TIER_DROP => TIERS[index - TIER_DROP],
		_ => STARTING_RANK,
	}
}

/// The best rank the account has ever held.
///
/// Not the same as its best character's rank right now. A character is demoted
/// when it loses, but the account level rank at `ACCOUNT_RANK` only ever goes
/// up - across the whole population it is above the best character in 132
/// accounts out of 544 and below it in none - so it is the account's high
/// water mark and the characters are where it stands today.
///
/// The floor is what the player has earned, so it follows the high water mark.
/// Reading only the characters would drop the floor for exactly the players
/// who have the longest history: among the 30 accounts with the most matches
/// played, 10 have a high water mark above their best character, by 3 to 8
/// ranks.
fn best_rank(record: &[u8]) -> u8 {
	let best_character = (0..CHAR_COUNT).map(|i| record[CHAR_BASE + i * CHAR_STRIDE + CHAR_RANK]).max().unwrap_or(0);
	best_character.max(record[ACCOUNT_RANK])
}

/// Raises every character below `floor`, returning how many were raised.
///
/// The account level rank comes along when it sits below the floor, which
/// happens where the floor is the starting rank and the account never reached
/// it. Leaving it behind would show the account ranked under every one of its
/// own characters. It only ever goes up here, as in the game.
fn raise_to(record: &mut [u8], floor: u8) -> usize {
	let points = rank_points_for(floor).to_be_bytes();
	let mut raised = 0;
	for i in 0..CHAR_COUNT {
		let base = CHAR_BASE + i * CHAR_STRIDE;
		if record[base + CHAR_RANK] >= floor {
			continue;
		}
		record[base + CHAR_RANK] = floor;
		record[base + CHAR_RANK_POINTS..base + CHAR_RANK_POINTS + 2].copy_from_slice(&points);
		raised += 1;
	}
	if record[ACCOUNT_RANK] < floor {
		record[ACCOUNT_RANK] = floor;
	}
	raised
}

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

/// Raises the ranks a save is entitled to, if any.
///
/// `previous` is the save this one replaces, or `None` for the account's first
/// save. A first save is brought up to the starting rank. Otherwise the ranks
/// are only touched when the account's best character has just crossed into a
/// new tier, so a character demoted below the floor since the last crossing
/// stays where it fell.
///
/// Returns the rewritten save, or `None` when nothing changed: another title,
/// an unexpected size, a checksum that does not verify, no tier crossed, or
/// every character already at or above the floor.
pub(crate) fn apply_rank_floor(com_id: &ComId, data: &[u8], previous: Option<&[u8]>) -> Option<Vec<u8>> {
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
		warn!("NPWR02973_00 save failed its checksum, rank floor not applied");
		return None;
	}

	let floor = floor_for(best_rank(data));

	// Past the first save, only act on the transition. A previous save that is
	// unreadable is treated as no previous save at all rather than guessed at.
	if let Some(previous) = previous {
		if previous.len() != RECORD_SIZE || checksum(previous) != stored_checksum(previous) {
			debug!("NPWR02973_00 previous save is unusable, rank floor not applied");
			return None;
		}
		if floor_for(best_rank(previous)) >= floor {
			return None;
		}
	}

	let mut out = data.to_vec();
	let raised = raise_to(&mut out, floor);
	if raised == 0 {
		return None;
	}

	let resealed = checksum(&out).to_be_bytes();
	out[0..4].copy_from_slice(&resealed);

	debug!(raised, floor, first_save = previous.is_none(), "NPWR02973_00 rank floor applied");
	Some(out)
}

/// Where the account totals live in the save.
const TOTAL_WINS: usize = 0xCDC;
const TOTAL_LOSSES: usize = 0xCE0;

/// How a match the account just finished went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOutcome {
	Won,
	Lost,
}

fn account_total(record: &[u8], offset: usize) -> u32 {
	u32::from_be_bytes([record[offset], record[offset + 1], record[offset + 2], record[offset + 3]])
}

/// Reads a match result out of the difference between two saves.
///
/// The server never sees the result of a match: play is peer to peer and the
/// title reports nothing about it. What it does see is the save written
/// afterwards, which carries the account's running totals, so one more win
/// than the save before it means the account won.
///
/// Returns `None` for a save that did not follow a match, which is most of
/// them - the title also re-uploads an unchanged record - and for anything it
/// cannot read with confidence.
pub fn match_outcome(com_id: &ComId, current: &[u8], previous: &[u8]) -> Option<MatchOutcome> {
	if com_id != &NPWR02973_00 {
		return None;
	}
	if current.len() != RECORD_SIZE || previous.len() != RECORD_SIZE {
		return None;
	}
	if checksum(current) != stored_checksum(current) || checksum(previous) != stored_checksum(previous) {
		return None;
	}

	let wins = account_total(current, TOTAL_WINS).checked_sub(account_total(previous, TOTAL_WINS))?;
	let losses = account_total(current, TOTAL_LOSSES).checked_sub(account_total(previous, TOTAL_LOSSES))?;

	// Exactly one of the two moved, by exactly one. Anything else is a save
	// that did not follow a single match, and guessing at it would be worse
	// than recording nothing.
	match (wins, losses) {
		(1, 0) => Some(MatchOutcome::Won),
		(0, 1) => Some(MatchOutcome::Lost),
		_ => None,
	}
}

/// The account's running totals, for reporting a record.
pub fn account_record(com_id: &ComId, record: &[u8]) -> Option<(u32, u32)> {
	if com_id != &NPWR02973_00 || record.len() != RECORD_SIZE || checksum(record) != stored_checksum(record) {
		return None;
	}
	Some((account_total(record, TOTAL_WINS), account_total(record, TOTAL_LOSSES)))
}

/// What one character of a save looked like before or after an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterRank {
	pub character: usize,
	pub rank: u8,
	pub rank_points: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
	/// The title does not store its ranks in a layout this knows about.
	UnsupportedTitle,
	/// Not a well formed save: wrong size, or the checksum does not verify.
	MalformedSave,
	/// There is no character with that id.
	NoSuchCharacter,
}

/// Number of characters a save has room for, so a caller can report the
/// accepted range without knowing the layout.
pub const CHARACTERS: usize = CHAR_COUNT;

/// Sets one character's rank, for an operator correcting a record.
///
/// Unlike the floor, this will lower a rank as well as raise it: the point of
/// it is to put a value back where it belongs. `rank_points` defaults to the
/// bottom of the new rank when not given.
///
/// Returns the rewritten save together with the character as it was, so the
/// caller can report what it replaced.
pub fn set_character_rank(com_id: &ComId, data: &[u8], character: usize, rank: u8, rank_points: Option<u16>) -> Result<(Vec<u8>, CharacterRank), EditError> {
	if com_id != &NPWR02973_00 {
		return Err(EditError::UnsupportedTitle);
	}
	if character >= CHAR_COUNT {
		return Err(EditError::NoSuchCharacter);
	}
	if data.len() != RECORD_SIZE || checksum(data) != stored_checksum(data) {
		return Err(EditError::MalformedSave);
	}

	let base = CHAR_BASE + character * CHAR_STRIDE;
	let previous = CharacterRank {
		character,
		rank: data[base + CHAR_RANK],
		rank_points: u16::from_be_bytes([data[base + CHAR_RANK_POINTS], data[base + CHAR_RANK_POINTS + 1]]),
	};

	let mut out = data.to_vec();
	out[base + CHAR_RANK] = rank;
	let points = rank_points.unwrap_or_else(|| rank_points_for(rank)).to_be_bytes();
	out[base + CHAR_RANK_POINTS..base + CHAR_RANK_POINTS + 2].copy_from_slice(&points);

	let resealed = checksum(&out).to_be_bytes();
	out[0..4].copy_from_slice(&resealed);

	Ok((out, previous))
}

/// Reads one character out of a save, so an operator can look before editing.
pub fn read_character_ranks(com_id: &ComId, data: &[u8]) -> Result<Vec<CharacterRank>, EditError> {
	if com_id != &NPWR02973_00 {
		return Err(EditError::UnsupportedTitle);
	}
	if data.len() != RECORD_SIZE || checksum(data) != stored_checksum(data) {
		return Err(EditError::MalformedSave);
	}

	Ok((0..CHAR_COUNT)
		.map(|character| {
			let base = CHAR_BASE + character * CHAR_STRIDE;
			CharacterRank {
				character,
				rank: data[base + CHAR_RANK],
				rank_points: u16::from_be_bytes([data[base + CHAR_RANK_POINTS], data[base + CHAR_RANK_POINTS + 1]]),
			}
		})
		.collect())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Builds a save whose checksum verifies, with every character at `rank`.
	/// A save whose whole roster sits at `rank`, with the account's high water
	/// mark at the same place, which is how a save that never demoted looks.
	fn make_record(rank: u8) -> Vec<u8> {
		make_record_with_account(rank, rank)
	}

	/// A save whose roster sits at `rank` while the account once reached
	/// `account_rank`, which is how a save looks after its characters demoted.
	fn make_record_with_account(rank: u8, account_rank: u8) -> Vec<u8> {
		let mut record = vec![0u8; RECORD_SIZE];
		for i in 0..CHAR_COUNT {
			record[CHAR_BASE + i * CHAR_STRIDE + CHAR_RANK] = rank;
		}
		record[ACCOUNT_RANK] = account_rank;
		reseal(&mut record);
		record
	}

	/// A save with one character the player climbed with and a roster sitting
	/// well below it, which is the shape the floor exists for.
	fn make_record_with_one_climber(climber: u8, rest: u8, account_rank: u8) -> Vec<u8> {
		let mut record = make_record_with_account(rest, account_rank);
		record[CHAR_BASE + CHAR_RANK] = climber;
		reseal(&mut record);
		record
	}

	fn reseal(record: &mut [u8]) {
		let sum = checksum(record);
		record[0..4].copy_from_slice(&sum.to_be_bytes());
	}

	fn rank_of(record: &[u8], character: usize) -> u8 {
		record[CHAR_BASE + character * CHAR_STRIDE + CHAR_RANK]
	}

	fn points_of(record: &[u8], character: usize) -> u16 {
		let base = CHAR_BASE + character * CHAR_STRIDE + CHAR_RANK_POINTS;
		u16::from_be_bytes([record[base], record[base + 1]])
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
	fn floor_sits_two_tiers_below() {
		for best in 0..=20 {
			assert_eq!(floor_for(best), 10, "best {}", best);
		}
		for best in 21..=24 {
			assert_eq!(floor_for(best), 13, "best {}", best);
		}
		for best in 25..=28 {
			assert_eq!(floor_for(best), 17, "best {}", best);
		}
		for best in 29..=32 {
			assert_eq!(floor_for(best), 21, "best {}", best);
		}
		for best in 33..=37 {
			assert_eq!(floor_for(best), 25, "best {}", best);
		}
		for best in 38..=40 {
			assert_eq!(floor_for(best), 29, "best {}", best);
		}
		assert_eq!(floor_for(41), 33);
		assert_eq!(floor_for(42), 33);
	}

	#[test]
	fn floor_never_exceeds_the_best_rank() {
		for best in 0..=42u8 {
			assert!(floor_for(best) <= best.max(STARTING_RANK), "best {}", best);
		}
	}

	#[test]
	fn rank_points_follow_the_kyu_ladder_and_reset_at_first_dan() {
		for rank in 1..10u8 {
			assert_eq!(rank_points_for(rank), 200 * rank as u16);
		}
		assert_eq!(rank_points_for(10), 0);
		assert_eq!(rank_points_for(13), 2300, "an observed rank uses its own value");
		let interpolated = rank_points_for(20);
		assert!(interpolated > 0, "an unobserved rank is interpolated, not zero");
	}

	#[test]
	fn a_first_save_is_brought_up_to_the_starting_rank() {
		let record = make_record(0);
		let out = apply_rank_floor(&NPWR02973_00, &record, None).expect("a first save is raised");
		for i in 0..CHAR_COUNT {
			assert_eq!(rank_of(&out, i), STARTING_RANK);
			assert_eq!(points_of(&out, i), STARTING_RANK_POINTS);
		}
		assert_eq!(checksum(&out), stored_checksum(&out), "the rewritten save reseals");
	}

	#[test]
	fn a_first_save_already_at_the_starting_rank_is_left_alone() {
		assert!(apply_rank_floor(&NPWR02973_00, &make_record(STARTING_RANK), None).is_none());
		assert!(apply_rank_floor(&NPWR02973_00, &make_record(STARTING_RANK + 5), None).is_none());
	}

	#[test]
	fn crossing_a_tier_raises_the_rest_of_the_roster() {
		// The account sat below the Warrior tier and has just reached it.
		let previous = make_record(STARTING_RANK);
		let mut current = make_record(STARTING_RANK);
		current[CHAR_BASE + CHAR_RANK] = 29;
		reseal(&mut current);

		let out = apply_rank_floor(&NPWR02973_00, &current, Some(&previous)).expect("the crossing raises the rest");
		assert_eq!(rank_of(&out, 0), 29, "the character that climbed is untouched");
		for i in 1..CHAR_COUNT {
			assert_eq!(rank_of(&out, i), 21, "character {}", i);
			assert_eq!(points_of(&out, i), rank_points_for(21));
		}
	}

	#[test]
	fn staying_inside_a_tier_changes_nothing() {
		let mut previous = make_record(STARTING_RANK);
		previous[CHAR_BASE + CHAR_RANK] = 29;
		reseal(&mut previous);

		let mut current = previous.clone();
		current[CHAR_BASE + CHAR_RANK] = 31; // same tier, so the floor is unchanged
		reseal(&mut current);

		assert!(apply_rank_floor(&NPWR02973_00, &current, Some(&previous)).is_none());
	}

	#[test]
	fn a_demoted_character_is_not_pulled_back_up() {
		// Both saves are in the same tier, and one character has dropped below
		// the floor since the last crossing. It must stay where it fell.
		let mut previous = make_record(21);
		previous[CHAR_BASE + CHAR_RANK] = 29;
		reseal(&mut previous);

		let mut current = previous.clone();
		current[CHAR_BASE + CHAR_STRIDE + CHAR_RANK] = 14;
		reseal(&mut current);

		assert!(apply_rank_floor(&NPWR02973_00, &current, Some(&previous)).is_none());
	}

	#[test]
	fn a_character_above_the_floor_keeps_its_rank_and_points() {
		let previous = make_record(STARTING_RANK);
		let mut current = make_record(STARTING_RANK);
		current[CHAR_BASE + CHAR_RANK] = 29;
		let kept = CHAR_BASE + 3 * CHAR_STRIDE;
		current[kept + CHAR_RANK] = 26;
		current[kept + CHAR_RANK_POINTS..kept + CHAR_RANK_POINTS + 2].copy_from_slice(&1234u16.to_be_bytes());
		reseal(&mut current);

		let out = apply_rank_floor(&NPWR02973_00, &current, Some(&previous)).expect("the rest is raised");
		assert_eq!(rank_of(&out, 3), 26, "a rank above the floor is left alone");
		assert_eq!(points_of(&out, 3), 1234);
		assert_eq!(rank_of(&out, 1), 21);
	}

	#[test]
	fn an_operator_can_set_one_character_rank() {
		let record = make_record(10);
		let (out, previous) = set_character_rank(&NPWR02973_00, &record, 14, 25, None).expect("the edit applies");
		assert_eq!(previous.character, 14);
		assert_eq!(previous.rank, 10, "the caller is told what it replaced");
		assert_eq!(rank_of(&out, 14), 25);
		assert_eq!(points_of(&out, 14), rank_points_for(25), "points default to the bottom of the new rank");
		assert_eq!(rank_of(&out, 13), 10, "no other character is touched");
		assert_eq!(checksum(&out), stored_checksum(&out), "the edited save reseals");
	}

	#[test]
	fn an_operator_can_lower_a_rank_and_choose_the_points() {
		let record = make_record(25);
		let (out, previous) = set_character_rank(&NPWR02973_00, &record, 0, 13, Some(4321)).expect("the edit applies");
		assert_eq!(previous.rank, 25);
		assert_eq!(rank_of(&out, 0), 13, "lowering is allowed, unlike the floor");
		assert_eq!(points_of(&out, 0), 4321);
	}

	#[test]
	fn an_edit_is_refused_for_an_unknown_character_or_an_unusable_save() {
		let record = make_record(10);
		assert_eq!(set_character_rank(&NPWR02973_00, &record, CHARACTERS, 25, None).unwrap_err(), EditError::NoSuchCharacter);
		assert_eq!(set_character_rank(b"NPWR00482_00", &record, 0, 25, None).unwrap_err(), EditError::UnsupportedTitle);

		let mut corrupt = record.clone();
		corrupt[0] ^= 0xFF;
		assert_eq!(set_character_rank(&NPWR02973_00, &corrupt, 0, 25, None).unwrap_err(), EditError::MalformedSave);
	}

	#[test]
	fn reading_ranks_reports_every_character() {
		let mut record = make_record(10);
		record[CHAR_BASE + 7 * CHAR_STRIDE + CHAR_RANK] = 29;
		reseal(&mut record);

		let ranks = read_character_ranks(&NPWR02973_00, &record).expect("a well formed save reads");
		assert_eq!(ranks.len(), CHARACTERS);
		assert_eq!(ranks[7].rank, 29);
		assert_eq!(ranks[0].rank, 10);
		assert!(ranks.iter().enumerate().all(|(i, c)| c.character == i));
	}

	#[test]
	fn a_win_and_a_loss_are_read_from_the_account_totals() {
		let previous = make_record(10);
		let mut won = previous.clone();
		won[TOTAL_WINS + 3] = 1;
		reseal(&mut won);
		assert_eq!(match_outcome(&NPWR02973_00, &won, &previous), Some(MatchOutcome::Won));

		let mut lost = previous.clone();
		lost[TOTAL_LOSSES + 3] = 1;
		reseal(&mut lost);
		assert_eq!(match_outcome(&NPWR02973_00, &lost, &previous), Some(MatchOutcome::Lost));
	}

	#[test]
	fn a_save_that_did_not_follow_a_match_reports_nothing() {
		let previous = make_record(10);

		// The title re-uploads an unchanged record more often than not.
		assert_eq!(match_outcome(&NPWR02973_00, &previous, &previous), None);

		// Both totals moving, or either moving by more than one, is not a
		// single match and must not be guessed at.
		let mut both = previous.clone();
		both[TOTAL_WINS + 3] = 1;
		both[TOTAL_LOSSES + 3] = 1;
		reseal(&mut both);
		assert_eq!(match_outcome(&NPWR02973_00, &both, &previous), None);

		let mut jumped = previous.clone();
		jumped[TOTAL_WINS + 3] = 5;
		reseal(&mut jumped);
		assert_eq!(match_outcome(&NPWR02973_00, &jumped, &previous), None);
	}

	#[test]
	fn a_total_going_backwards_reports_nothing() {
		let mut previous = make_record(10);
		previous[TOTAL_WINS + 3] = 4;
		reseal(&mut previous);

		let mut current = previous.clone();
		current[TOTAL_WINS + 3] = 2;
		reseal(&mut current);

		assert_eq!(match_outcome(&NPWR02973_00, &current, &previous), None);
	}

	#[test]
	fn the_account_record_reads_back() {
		let mut record = make_record(10);
		record[TOTAL_WINS + 2] = 1;
		record[TOTAL_WINS + 3] = 44;
		record[TOTAL_LOSSES + 3] = 7;
		reseal(&mut record);
		assert_eq!(account_record(&NPWR02973_00, &record), Some((300, 7)));
		assert_eq!(account_record(b"NPWR00482_00", &record), None);
	}

	#[test]
	fn the_floor_follows_what_the_account_once_reached() {
		// A player who climbed to 29 and whose characters have since fallen to
		// 22 is entitled to the floor for 29, not the one for 22. This is the
		// case for a third of the accounts with the most matches played.
		let previous = make_record_with_one_climber(22, 10, 28);
		let current = make_record_with_one_climber(22, 10, 29);

		let raised = apply_rank_floor(&NPWR02973_00, &current, Some(&previous)).expect("crossing 29 raises the roster");
		assert_eq!(rank_of(&raised, 1), 21, "floor for 29 is 21, not the 13 that reading only the characters would give");
		assert_eq!(rank_of(&raised, 0), 22, "the character the player climbed with is untouched");
	}

	#[test]
	fn the_account_rank_comes_up_with_the_roster() {
		// Below 1st Dan the whole roster is raised to it. Leaving the account
		// behind would rank it under every one of its own characters.
		let record = make_record_with_account(3, 3);
		let raised = apply_rank_floor(&NPWR02973_00, &record, None).expect("a first save is brought up to the starting rank");

		assert_eq!(rank_of(&raised, 0), STARTING_RANK);
		assert_eq!(raised[ACCOUNT_RANK], STARTING_RANK);
	}

	#[test]
	fn an_account_rank_above_the_floor_is_left_alone() {
		let previous = make_record_with_one_climber(22, 10, 28);
		let current = make_record_with_one_climber(22, 10, 29);
		let raised = apply_rank_floor(&NPWR02973_00, &current, Some(&previous)).unwrap();
		assert_eq!(raised[ACCOUNT_RANK], 29, "the high water mark is never pulled down to the floor");
	}

	#[test]
	fn ignores_other_titles() {
		assert!(apply_rank_floor(b"NPWR00482_00", &make_record(0), None).is_none());
	}

	#[test]
	fn ignores_an_unexpected_size() {
		assert!(apply_rank_floor(&NPWR02973_00, &vec![0u8; 16], None).is_none());
	}

	#[test]
	fn refuses_a_save_whose_checksum_does_not_verify() {
		let mut record = make_record(0);
		record[0] ^= 0xFF;
		assert!(apply_rank_floor(&NPWR02973_00, &record, None).is_none());
	}

	#[test]
	fn refuses_to_guess_when_the_previous_save_is_unusable() {
		let mut current = make_record(STARTING_RANK);
		current[CHAR_BASE + CHAR_RANK] = 29;
		reseal(&mut current);

		let mut corrupt_previous = make_record(STARTING_RANK);
		corrupt_previous[0] ^= 0xFF;

		assert!(apply_rank_floor(&NPWR02973_00, &current, Some(&corrupt_previous)).is_none());
		assert!(apply_rank_floor(&NPWR02973_00, &current, Some(&vec![0u8; 16])).is_none());
	}
}
