use crate::server::client::ComId;
use crate::server::stream_extractor::np2_structs::SearchRoomRequest;
use tracing::debug;

const NPWR02973_00: ComId = *b"NPWR02973_00";
const RANK_LOWER_FILTER_INDEX: usize = 3;
const RANK_UPPER_FILTER_INDEX: usize = 4;
const FIRST_DAN_MAX_RANK: u32 = 10;

/// Builds the effective ranked-match search request for NPWR02973_00.
///
/// The request sent by the client is never modified. For 2nd-dan-or-higher
/// players, the two rank-range filter values are moved two ranks toward their
/// midpoint and clamped at that midpoint. 1st-dan players retain the range
/// selected by the client.
pub(crate) fn build_npwr02973_adjusted_request(com_id: &ComId, search_req: &SearchRoomRequest) -> Option<SearchRoomRequest> {
	if com_id != &NPWR02973_00 {
		return None;
	}

	let lower = search_req.int_filter.get(RANK_LOWER_FILTER_INDEX)?.attr.as_ref()?.num;
	let upper = search_req.int_filter.get(RANK_UPPER_FILTER_INDEX)?.attr.as_ref()?.num;
	if lower > upper {
		return None;
	}

	let rank = lower + (upper - lower) / 2;
	if rank <= FIRST_DAN_MAX_RANK {
		debug!(rank, int_filter_3 = lower, int_filter_4 = upper, "NPWR02973_00 rank search range left unchanged for 1st dan");
		return None;
	}

	let adjusted_lower = lower.saturating_add(2).min(rank);
	let adjusted_upper = upper.saturating_sub(2).max(rank);
	debug!(
		rank,
		int_filter_3 = lower,
		int_filter_4 = upper,
		adjusted_int_filter_3 = adjusted_lower,
		adjusted_int_filter_4 = adjusted_upper,
		"NPWR02973_00 rank search range evaluated"
	);
	if (adjusted_lower, adjusted_upper) == (lower, upper) {
		return None;
	}

	let mut adjusted_request = search_req.clone();
	adjusted_request.int_filter.get_mut(RANK_LOWER_FILTER_INDEX)?.attr.as_mut()?.num = adjusted_lower;
	adjusted_request.int_filter.get_mut(RANK_UPPER_FILTER_INDEX)?.attr.as_mut()?.num = adjusted_upper;

	Some(adjusted_request)
}
