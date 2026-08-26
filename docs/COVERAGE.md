# Test-coverage matrix

Level-by-level coverage of the yodel crate against five categories.
Every cell names at least one real, passing test.

## Legend

- **encode-KAT**: an exact known-answer vector on the encode/build
  side. The test pins the exact output bytes/samples for a fixed
  input.
- **decode-KAT**: an exact known-answer vector on the parse/decode
  side. A pinned wire input decodes to pinned field values.
- **roundtrip**: encode → decode returns the original value, often
  swept over variants, quadrants, rates or random-but-seeded inputs.
- **edge**: boundary and capacity cases, among them max/min
  coordinates, SSID 0/15, empty payloads, buffer limits and
  fractional samples-per-bit.
- **reject**: malformed input produces a *typed* error and never a
  panic.

Cells cite `file::test_name`, or `file::module` where a group of tests
lives in one module (`tests/edge_cases.rs::crc16_x25_catalogue` and its
four siblings are modules rather than single tests: five modules
holding fifteen tests). `src/...` names an in-module `#[cfg(test)]`
unit test; `tests/...` an integration test. Every citation in this
document is checked mechanically against `--list --include-ignored`
output from the compiled test binaries; none of it is verified by eye.
The checker is `scripts/check-coverage-citations.sh`, and CI runs it, so
a renamed or deleted test cannot leave a citation behind that reads as
evidence and is not.

> **Physical-layer measurement suites** sit outside this matrix because
> they measure *quality*, not correctness of a wire format:
> `tests/ber.rs` (raw bit error rate per mode, and the 50%-frame-recovery
> threshold in dB), `tests/false_positives.rs` (receiver specificity and
> false-accept exposure), `tests/noise.rs` and `tests/snr.rs`
> (sensitivity ladders). See `docs/BENCHMARKS.md`.

For purely directional layers (e.g. the modulator has no "decode" of
its own) the counterpart-KAT cell cites the pinned vector of the paired
direction that exercises it. For total codes with no rejectable input
(NRZI), the reject cell cites the test documenting that totality.

## Totals, by tier

MEASURED at HEAD. `cargo test --all-features`: **1306 passed, 0 failed,
62 ignored**, across 57 test binaries plus the doctest pass. Running
every compiled test binary with `--list --include-ignored` counts
**1199 test functions**; the doctest pass reports **169 doctests**
(162 run, 7 ignored). 1368 cases in all.

The per-tier split below was last derived at commit `515f8ba`, when the
totals were 1159 passed / 63 ignored over 1063 test functions and 159
doctests. The 23 cases added since are all tier 1-2, so the ignored
column is unchanged; re-derive the split before quoting it as current.

`cargo test --all-features -- --ignored` is **also green**, which is new
and is the point of "Tier-3/4 guard hardening" below: with the
`YODEL_REF_*` variables unset, the 46 tier-4 tests all skip cleanly
(seventeen of them used to *fail*), the 6 tier-3 tests run against the
`corpus/` present here, the cost-gated sweep runs, and the 10 README
fences compile and pass. 63 passed, 0 failed.

Tiers are `CONTRIBUTING.md`'s (1–2 hermetic and CI-gating, 3 needs
`corpus/`, 4 needs the external reference binaries):

| Tier | Needs | Cases | Where |
|---|---|---|---|
| 1–2 | nothing | 1156 fns + 164 doctests = **1320** | everything `cargo test --all-features` runs; the only tier CI gates |
| 3 | `corpus/` or `scratch/` WAVs | 8 | `tests/corpus_aprs.rs` (3), `tests/benchmark.rs` (4), `tests/cli.rs` (1) |
| 4 | the external reference binaries | 46 | `tests/oracle.rs` (31), `tests/differential.rs` (5), `tests/wspr_differential.rs` (3), `tests/ft8_differential.rs` (3), `tests/aprs_differential.rs` (2; one of them also wants `corpus/`), `tests/il2p_differential.rs` (2) |
| — | nothing; ignored on runtime cost alone | 1 | `tests/ber.rs::ber_curve_fine_sweep` (dense 0.5 dB-step sweep) |
| — | nothing; not compiled by a default run | 7 | the `rust,ignore` fences: 4 in `README.md`, 3 in `docs/EMBEDDED.md`; see the doctest note below |

The ignored total is external material, one cost-gated sweep, and the
`rust,ignore` fences. That is the whole of the gap between the passed
count and the case count: 1320 passed, 8 + 46 + 1 = 55 ignored in
`tests/`, 7 ignored fences, 62 ignored in all.

Every number in that table is derivable from one run and one `grep`, and
none of them were, before the run that produced this revision. The
previous version undercounted tier 3 by two whole files, missed
`tests/cli.rs` entirely, and quoted a fence count that no longer
matched the sources. Re-derive rather than edit: the test totals come
from aggregating `^test result:` over `cargo test --all-features`, and
the per-file ignore counts from
`grep -c '^[[:space:]]*#\[ignore' tests/*.rs` -- anchored, or it also
counts doc comments that merely mention the attribute.

### What moved, per file

The previous baseline this document was written against was 1094 passed
/ 63 ignored; by commit `ef2f285`, the last revision before the two APRS
commits below, it was **1099 passed / 63 ignored**, made up of 1012
test functions and 150 doctests. Both ends of the table are counted the
same way, with `--list --include-ignored` against the compiled binaries
of each revision, so the delta below is a subtraction of two
measurements:

| File | was | now | what landed |
|---|---:|---:|---|
| `tests/symbol_from_address.rs` | 0 | 15 | **new file**: chapter 20's address-borne symbols, with Appendix 2 transcribed as an independent oracle |
| `tests/aprs_extras.rs` | 25 | 34 | reply-ACK accessors, luminosity mid-block, the second tagged `s`, the tagged `c`, the trailer before any tag |
| `tests/mic_e.rs` | 17 | 25 | the device prefix decoupled from the altitude, and `Decoded::decode_frame` |
| `tests/decoded_laws.rs` | 8 | 13 | the four frame-level laws, destination independence among them |
| `tests/aprs.rs` | 12 | 15 | zero speed beside a real course |
| `src/aprs/extension.rs` (in-module) | 10 | 14 | the `000/000` sentinel belongs to the pair, not to each half |
| `src/aprs/message.rs` (in-module) | 7 | 11 | `reply_ack` / `acked_number` |
| `src/aprs/weather.rs` (in-module) | 10 | 13 | the tagged `c`, luminosity, and the second `s` in the positionless layout |
| doctests | 150 | 159 | one per new public item: `Decoded`, `decoded`, `as_padded`, `reply_ack`, `acked_number`, `snowfall`, and the three `symbol::` lookups |

+51 test functions and +9 doctests, which is exactly 1099 → 1159 and
1012 → 1063. The ignored count did not move, because nothing added here
is tier-3 or tier-4. Both commits were validated against the corpus and
the reference decoder, but every law they produced is *hermetic*, so CI
gates all of it.

## Matrix

| Layer | encode-KAT | decode-KAT | roundtrip | edge | reject |
|---|---|---|---|---|---|
| AFSK modulate | `tests/coverage_fill.rs::afsk_modulate_exact_pcm_kat_48k` (all 120 samples pinned), `src/modulator.rs::mark_tone_48k_first_16_samples_pinned` | `tests/roundtrip.rs::pinned_bits_for_known_transmission` (pinned bit recovery of the modulated output) | `tests/roundtrip.rs::byte_patterns_roundtrip_i16_48000`, `tests/roundtrip.rs::i16_and_f32_paths_agree` | `tests/coverage_fill.rs::afsk_modulate_exact_pcm_prefix_44100` (fractional samples/bit), `tests/roundtrip.rs::modulator_pinned_bit_lengths_8000`, `tests/roundtrip.rs::sample_rate_boundary_matrix` | `src/modulator.rs::config_baud_exceeds_sample_rate_rejected`, `tests/roundtrip.rs::display_of_real_constructor_errors` |
| AFSK demodulate | `tests/coverage_fill.rs::afsk_modulate_exact_pcm_kat_48k` (pinned input vector) | `tests/roundtrip.rs::pinned_bits_for_known_transmission`, `tests/roundtrip.rs::pinned_bits_for_pure_mark_tone` | `tests/roundtrip.rs::all_ones_payload_all_rates_i16`, `tests/noise.rs::long_payloads_20db_all_rates` (seeded-noise SNR pins) | `tests/roundtrip.rs::demodulator_silence_is_stable`, `tests/roundtrip.rs::demodulator_dc_input_is_stable`, `tests/roundtrip.rs::demodulator_out_of_band_tone_is_stable` | `src/discriminator.rs::rejects_one_sample_per_bit` (typed `ConfigError`), `src/slicer.rs::construction_rejects_low_sample_rate` |
| NRZI encode | `tests/nrzi.rs::all_zeros_yields_alternating_line` (exact line pattern) | `tests/nrzi.rs::constant_line_decodes_to_all_ones_after_first_bit` | `tests/nrzi.rs::roundtrip_assorted_sequences_both_initial_states` | `tests/nrzi.rs::all_ones_stall_constant_line_level` | `tests/coverage_fill.rs::nrzi_totality_no_invalid_inputs` (total code: no rejectable input, documented) |
| NRZI decode | `tests/nrzi.rs::all_zeros_yields_alternating_line` | `tests/nrzi.rs::constant_line_decodes_to_all_ones_after_first_bit` | `tests/nrzi.rs::free_function_adapters_roundtrip_with_default_state`, `src/nrzi.rs::roundtrip_pseudo_random_sequences` | `tests/nrzi.rs::decoder_self_synchronizes_after_at_most_one_bit` | `tests/coverage_fill.rs::nrzi_totality_no_invalid_inputs` |
| HDLC stuff | `src/ax25/hdlc.rs::frame_bits_layout_matches_reference_stuffing` (exact bit layout), `src/ax25/hdlc.rs::stuffing_after_exactly_five_ones` | `src/ax25/hdlc.rs::payload_flag_byte_is_stuffed_away` | `src/ax25/hdlc.rs::roundtrip_through_deframer` | `src/ax25/hdlc.rs::five_ones_at_end_of_data_still_stuffed`, `src/ax25/hdlc.rs::back_to_back_frames_share_a_flag` | `src/ax25/hdlc.rs::garbage_bits_never_panic_and_yield_nothing_valid` |
| HDLC unstuff | `src/ax25/hdlc.rs::frame_bits_layout_matches_reference_stuffing` (pinned input) | `src/ax25/hdlc.rs::roundtrip_through_deframer` (known stuffed stream → exact frame bytes) | `src/ax25/hdlc.rs::roundtrip_stuffing_heavy_payload`, `tests/common/mod.rs::stuff_destuff_round_trip` | `src/ax25/hdlc.rs::runt_frames_discarded_silently`, `src/ax25/hdlc.rs::abort_sequence_discards_frame_in_progress` | `src/ax25/hdlc.rs::corrupted_fcs_is_reported`, `src/ax25/hdlc.rs::oversize_frame_is_reported` |
| AX.25 address | `src/ax25/addr.rs::encode_known_layout` (exact 7 wire bytes), `src/ax25/addr.rs::encode_source_last_with_ssid` | `tests/coverage_fill.rs::ax25_address_decode_kat` | `src/ax25/addr.rs::round_trip_ssid_extremes` | `src/ax25/addr.rs::ssid_boundaries` (SSID 0/15/16), `src/ax25/addr.rs::callsign_rejects_bad_lengths` | `src/ax25/addr.rs::callsign_rejects_bad_chars`, `src/ax25/addr.rs::decode_rejects_garbage` |
| AX.25 FCS | `src/ax25/fcs.rs::check_value` (`"123456789"` → `0x906E`), `src/ax25/fcs.rs::known_vectors`, `tests/edge_cases.rs::crc16_x25_catalogue` (published CRC RevEng check + residue values) | `tests/ax25.rs::crc_check_value` | `src/ax25/fcs.rs::streaming_matches_one_shot` | `src/ax25/fcs.rs::finish_is_non_consuming`, `src/ax25/fcs.rs::default_equals_new` | `tests/ax25.rs::corrupted_fcs_is_rejected` (typed `FcsMismatch`) |
| AX.25 frame build | `src/ax25/frame.rs::wire_layout_control_and_pid` (exact control/PID bytes) | `src/ax25/frame.rs::build_parse_round_trip_with_path` (built bytes reparsed field-exact) | `tests/ax25.rs::frame_build_parse_round_trip` | `src/ax25/frame.rs::max_digipeaters_round_trip`, `tests/coverage_fill.rs::ax25_empty_info_round_trip` | `src/ax25/frame.rs::build_rejects_small_buffer`, `src/ax25/frame.rs::with_path_rejects_too_many` |
| Digipeat relay, as **laws** rather than cases | — the relay writes no fixed wire form; the counterpart is `tests/digipeat_laws.rs::a_relay_carries_the_payload_byte_for_byte`, which pins the payload against the bytes that arrived | `src/digipeat.rs` doctest (the chapter-conventional `WIDE2-2` → `MYCALL*,WIDE2-1` mutation) | `tests/digipeat_laws.rs::a_relay_carries_the_payload_byte_for_byte`: 1638 relays over 269 payloads × 6 path shapes, driven heard-bytes-to-sent-bytes. The payloads include every single-byte data type identifier, three spellings this crate canonicalises away, and bytes no parser here accepts — a relay that transformed only what it understood would be a filter, and a relay that re-serialised the payload passes every path assertion while failing this | `tests/digipeat_laws.rs::every_relay_spends_exactly_one_hop_of_the_budget` sweeps 4900 path shapes and asserts the residual hop budget falls by **exactly one** per relay, which is what bounds a flood; `relaying_our_own_relay_is_always_declined` pins local loop freedom. In-module: the 19 branch tests in `src/digipeat.rs` | `src/digipeat.rs::fully_used_path_never_relayed`, `non_matching_first_hop_ignored`, `wide_n_zero_refused`, `wide_remaining_above_class_refused`, `wide_above_limit_refused`, `insertion_refused_when_path_full`, `non_wide_callsigns_are_not_pattern_matched` |
| AX.25 frame parse | `src/ax25/frame.rs::wire_layout_control_and_pid` (pinned input bytes) | `src/ax25/frame.rs::build_parse_round_trip_no_path` | `tests/ax25.rs::hdlc_round_trip_without_dsp` | `tests/ax25.rs::oversize_frame_is_rejected` (capacity limit) | `src/ax25/frame.rs::parse_rejects_short`, `parse_rejects_bad_control_and_pid`, `parse_rejects_unterminated_address_field`, `parse_rejects_truncated_after_addresses` |
| KISS encode | `tests/kiss.rs::encode_vectors_escaping` (exact FEND/FESC/TFEND/TFESC bytes), `tests/kiss.rs::command_byte_for_every_variant_and_port` | `tests/kiss.rs::iterator_encoder_equals_buffer_encoder` | `tests/kiss.rs::round_trip_sweep_all_byte_values` | `tests/kiss.rs::port_validation` (ports 0/15/16), `tests/kiss.rs::constants_documented_values` | `tests/kiss.rs::encode_buffer_too_small` |
| KISS decode/deframe | `tests/kiss.rs::encode_vectors_escaping` (pinned wire input) | `tests/kiss.rs::streaming_decode_across_split_pushes`, `tests/kiss.rs::return_frame_decodes` | `tests/kiss.rs::escaped_pair_in_payload_survives_round_trip` | `tests/kiss.rs::empty_frames_and_garbage_before_fend` | `tests/kiss.rs::invalid_escape_error_then_next_frame_ok`, `dangling_escape_at_frame_end_is_invalid`, `overflow_error_then_next_frame_ok`, `unknown_command_byte_reported_on_decode`, `unknown_command_nibbles_rejected` |
| APRS position uncompressed | `tests/aprs.rs::uncompressed_position_south_east_round_trip` (exact wire string), `src/aprs/position.rs::spec_uncompressed_vector` | `tests/aprs.rs::uncompressed_position_spec_vector_round_trip` (ch. 6 spec vector) | `src/aprs/position.rs::southern_eastern_hemispheres_round_trip` | `tests/coverage_fill.rs::position_boundary_extremes_round_trip` (±90/±180, one-past typed errors) | `tests/aprs.rs::rejection_vectors`, `src/aprs/position.rs::parse_rejections`, `tests/aprs.rs::builder_overflow` |
| APRS position **ambiguity** (ch. 6) | `tests/rebuild_fidelity.rs::space_blanked_coordinates_round_trip` (three real packets, blanked to the minute and to the tenth, rebuilt byte for byte) | `src/geo.rs::ambiguity_masks_to_the_chapter_6_levels`, which converts the spec's own prose ("nearest tenth of a minute", "nearest minute", "nearest ten minutes", "nearest degree") into independent arithmetic rather than calling `Ambiguity::step` | `space_blanked_control_packets_are_a_separate_population`: the half of the control group that was rejected only for a space, now parsing, with its declared level pinned per packet | `src/geo.rs::ambiguity_masking_is_idempotent_and_ordered` (both hemispheres, every level, masking never increases a magnitude and coarser levels absorb finer ones), and **`tests/cli.rs::position_ambiguity_is_honoured_by_both_renderers`**, which drives the built binary because the library was right and every renderer read the fields instead | `src/aprs/position.rs` rejects a space that is not part of a right-aligned run, at the exact offset; the compressed path never reaches this code, because a space there is the `cs` no-data trailer |
| APRS position compressed | `tests/compressed.rs::timestamped_compressed_wire_bytes` (exact bytes), `src/aprs/position.rs::spec_compressed_vector` | `tests/aprs.rs::compressed_position_spec_vector` (ch. 9 `/5L!!<*e7>` vector), `tests/compressed.rs::spec_course_speed_vector`, `spec_radio_range_vector`, `spec_altitude_vector` | `tests/compressed.rs::cs_round_trips_all_variants_and_quadrants`, `tests/aprs.rs::compressed_position_round_trip_hemispheres` | `tests/compressed.rs::cs_boundary_values_round_trip`, `large_altitude_is_wire_stable`, `course_rounds_to_nearest_step`, `compression_type_byte_round_trips`; **KNOWN GAP, pinned not fixed**: `compression_type_byte_drops_bit_6_known_gap`. `CompressionType::from_byte` reads no bit 6 and `to_byte` never sets one, so every wire byte in `64..=90` re-encodes as the value 64 lower. The test asserts today's *lossy* behaviour across the whole affected range, so a change fails loudly. Pinning it is not an endorsement: the byte-exactness invariant is broken for any parse-then-forward path | `tests/compressed.rs::rejects_bad_base91_in_trailer`, `rejects_truncated_trailer`, `rejects_out_of_range_builds` |
| APRS data extension (`CSE/SPD`, wind, `PHG`/`PHGR`, `RNG`, `DFS`) | `src/aprs/extension.rs::wire_round_trip_is_byte_exact` (17 wire forms, every one reproduced byte for byte) | `src/aprs/extension.rs::phg_code_tables` (spec power/height/gain/directivity tables), `phgr_needs_the_mandatory_slash` (the spec's own `PHG72604/` example), `range_and_dfs`, `zero_speed_is_only_unknown_as_a_whole_pair` (fixed defect: chapter 7 states the `000/000` sentinel for the **pair**, and the parser collapsed each half independently, so `315/000` lost a real speed of zero. That hit 16 corpus frames, and the differential's speed gap fell 26 → 10 when the fix landed) | `src/aprs/extension.rs::wire_round_trip_is_byte_exact` (parse → write, both symbol classes), `every_ddd_sss_round_trips_byte_exactly` (a law, not a table: all 1000 × 1000 numeric spellings × both symbol readings write back byte-identically or are rejected as bearings above 360, plus the dotted and blank spellings) and `tests/aprs.rs::every_ddd_sss_position_round_trips_byte_exactly` (the same law inside a whole position report) | `src/aprs/extension.rs::height_codes_above_nine` (`:` = 10240 ft, for balloons), `unknown_spellings_are_preserved_and_distinguished` (`000`/`...`/spaces), `wind_versus_course_depends_on_the_symbol`, `wind_reads_zero_the_same_way_the_weather_decoder_does` (the same seven bytes meant "calm" on the weather path and "unknown" here: one wire form with two readings inside one crate), `the_other_extensions_have_no_zero_collapse`, `tests/aprs.rs::zero_speed_beside_a_real_course_survives`, `wind_extension_and_weather_report_agree_on_calm` | `src/aprs/extension.rs::plain_text_is_not_an_extension` (incl. `Hwy/101` and `KG6/W6ABC`, the comments a loose parser destroys), `out_of_range_bearing_is_not_an_extension` |
| APRS `/A=` altitude | — (a view of `comment`, never re-serialised) | `src/aprs/extension.rs::altitude_anywhere_in_the_comment_including_negative` | `tests/aprs.rs` position round trips (bytes preserved in `comment`) | same test: found at start/middle/end, and the `/A=-ddddd` negative form | same test: 5-digit and non-digit forms rejected |
| APRS position timestamped | `tests/compressed.rs::timestamped_uncompressed_wire_bytes`, `timestamped_compressed_wire_bytes` | `tests/compressed.rs::timestamped_uncompressed_wire_bytes` (parses pinned bytes back) | `tests/compressed.rs::timestamped_positions_round_trip_both_dtis`, `tests/aprs.rs::timestamped_position_round_trip` | `tests/compressed.rs::timestamped_positions_round_trip_both_dtis` (all `Timestamp` forms × `/`/`@`) | `tests/compressed.rs::rejects_bad_timestamp` |
| Message | `src/aprs/message.rs::message_with_id_round_trip` (exact `:addr :text{id}` bytes) | `tests/aprs.rs::message_with_id_round_trip`, `tests/aprs.rs::ack_round_trip`, `tests/aprs_extras.rs::reply_ack_splits_the_chapter_14_forms` and `acked_number_pulls_the_mm_out_of_ack_and_rej` (the 1.1 reply-ACK `{MM}AA`, split into its two fields rather than passed through as opaque text), `reply_ack_decodes_the_off_air_qso` (the corpus frames that once failed as "message id length 6") | `src/aprs/message.rs::ack_and_rej_round_trip`, `message_without_id_round_trip`, `tests/aprs_extras.rs::reply_ack_needs_no_build_support` (the point of making reply-ACK an *accessor* pair rather than a variant: `build` is unchanged, so the byte-exact round trip holds by construction) | `src/aprs/message.rs::ack_like_text_with_brace_is_text`, `addressee_validation`, `reply_ack_splits_the_spec_forms`, `acked_number_pulls_out_the_mm` | `src/aprs/message.rs::parse_rejections`, `build_overflow_and_bad_id`, `tests/aprs.rs::rejection_vectors` (addressee/id), `src/aprs/message.rs::reply_ack_degenerate_ids_do_not_panic` and `accessors_are_total_on_hand_built_content` (the accessors are total over ids no parser would produce), `tests/aprs_extras.rs::reply_ack_degenerate_ids_round_trip_and_never_panic`, and the pair that pins what is **not** an identifier: `src/aprs/message.rs::a_brace_or_ack_that_is_not_an_id_stays_in_the_text` (nine shapes, both arms, including `{123456` one past the five-character boundary) with `a_valid_ack_or_rej_is_unaffected` beside it, plus `tests/rebuild_fidelity.rs::message_text_may_open_with_a_brace` and `message_text_may_open_with_ack` on real packets. Chapter 14 caps the identifier at five characters, so a longer run is text; rejecting the message instead lost 203 captured packets from 24 senders |
| Status | `src/aprs/status.rs::round_trip` (exact `>text` bytes) | `src/aprs/status.rs::round_trip` (reparse), `tests/cli.rs::decode_wav_written_by_library` (status through the full stack) | `tests/aprs.rs::status_round_trip` | `src/aprs/status.rs::empty_text_round_trips`, `tests/coverage_fill.rs::status_rejections_via_packet_dispatch` (bare `>` minimal form) | `src/aprs/status.rs::rejections`, `build_overflow`, `tests/coverage_fill.rs::status_rejections_via_packet_dispatch` |
| Weather | `tests/aprs_extras.rs::positionless_weather_spec_vector` (exact `_`+MDHM+fields bytes), `src/aprs/weather.rs::write_value_and_temperature_layout`, `tests/aprs_extras.rs::complete_weather_snowfall_round_trips_byte_for_byte` (3 pinned `sNNN`-bearing forms, plain and timestamped, rebuilt byte for byte) | `tests/aprs_extras.rs::positionless_weather_spec_vector`, `position_with_weather_spec_vector`, `complete_weather_tagged_s_is_snowfall_not_wind_speed` (fixed defect: the tagged `s` of a Complete Weather Report used to be read as wind speed and silently overwrite the correctly-decoded positional knots; VERIFIED wrong before the fix, and 0 such frames exist in `corpus/`, so only a hand-written vector could see it), `positionless_weather_second_tagged_s_is_snowfall` (the sibling: the `ssss` slot is spent **once**, so the *second* `s` of a positionless report is snow too, and the first fix gated on the layout, which is the wrong question), `complete_weather_tagged_c_leaves_the_positional_wind_direction` (fixed defect: `220/004c123` rebuilt as `_123/004`; chapter 12 gives `c` no second meaning, so the scan ends and the bytes go to `rest`), `weather_luminosity_recovers_the_rest_of_the_block` (fixed defect: an unknown tag mid-block cost every field *after* it, so `r000L050p000P000h50b09900` lost `p`, `P`, `h` and `b` to `rest`) | `tests/aprs_extras.rs::positionless_weather_all_missing_round_trip`, `position_with_weather_missing_wind_round_trip`, `positionless_weather_snowfall_builds_and_round_trips` (`build` now emits the trailing `sNNN` instead of refusing it) | `tests/aprs_extras.rs::weather_temperature_boundaries_round_trip`, `weather_negative_temperature_and_humidity_00`, `src/aprs/weather.rs::humidity_wire_convention`, `mdhm_bounds`, `tagged_s_is_wind_in_one_layout_and_snow_in_the_other` (one wire spelling, two measurements, decided by the layout), `snowfall_wire_inches_round_half_away_from_zero`, `tests/aprs_extras.rs::positionless_weather_tagged_s_is_still_wind_speed_in_mph` (mph here, knots there: the regression a careless fix would cause), `src/aprs/weather.rs::second_tagged_s_is_snow_in_the_positionless_layout_too`, `tagged_c_ends_the_scan_in_the_complete_layout`, `luminosity_is_read_mid_block_and_spells_itself_back` (the `l` tag means "1000 more than the digits", so the letter is a function of the value and the rebuild needs no memory of which arrived), `tests/aprs_extras.rs::complete_weather_tolerates_a_trailer_before_any_tag` (an over-strictness nobody had listed: a manufacturer stamp as the *first* post-wind token used to discard the whole typed report) | `tests/aprs_extras.rs::positionless_weather_rejections`, `positionless_weather_build_rejections`, `position_with_weather_rejections`, `src/aprs/weather.rs::parse_value_digits_dots_and_spaces`, `tagged_scanner_stops_at_unknown_trailer`, `range_checks_on_build`. **Not** covered, because it is not a rejection any more: `positionless_weather_cannot_carry_snowfall` pinned a typed `BadWeatherValue` on build, which was the right answer only while the second `s` was misread as wind; it went with that fix |
| Telemetry | `src/aprs/telemetry.rs::build_known_answer` (exact `T#SEQ,...` bytes) | `src/aprs/telemetry.rs::parse_known_answer`, `tests/aprs_extras.rs::telemetry_spec_vector_round_trip` | `tests/aprs_extras.rs::telemetry_boundary_sequences_round_trip` (seq 0/1/999/1000/1812/46 144/99 999, the four- and five-digit forms real trackers emit, where a fixed three-digit build would report 1812 as 812) | `src/aprs/telemetry.rs::sequence_boundaries_round_trip`; **the digital-field hazard**: `tests/rebuild_fidelity.rs::telemetry_two_analog_channels_and_a_digital_byte` pins that a report with two analog channels does not read its trailing `00000000` as `analog[2]`, which is the one relaxation in this effort that could make the crate *worse* rather than leave it unchanged, and `telemetry_short_field_count_finds_the_digital_field` pins the same shape from the rejecting side. MEASURED: 56 captured reports have this shape and **zero** offer two digital-field candidates | `tests/aprs_extras.rs::telemetry_rejections`, `src/aprs/telemetry.rs::parse_rejections`, `build_rejections` |
| Object | `tests/aprs_extras.rs::object_spec_vector_round_trip` (exact `;name*ts...` bytes), `src/aprs/object.rs::timestamp_write_known_answer` | `tests/aprs_extras.rs::object_spec_vector_round_trip`, `src/aprs/object.rs::timestamp_parse_all_formats` | `tests/aprs_extras.rs::object_killed_and_timestamp_formats_round_trip` | `tests/aprs_extras.rs::object_single_char_name_pads_to_nine`, `src/aprs/object.rs::name_scanner_strips_padding_and_validates`, `dhm_hms_range_helpers` | `tests/aprs_extras.rs::object_rejections`, `src/aprs/object.rs::timestamp_parse_rejections`, `build_name_char_rules` |
| Item | `tests/aprs_extras.rs::item_spec_vector_round_trip` (exact `)name!...` bytes) | `tests/aprs_extras.rs::item_spec_vector_round_trip` | `tests/aprs_extras.rs::item_killed_round_trip` | `tests/aprs_extras.rs::item_name_length_boundaries_round_trip` (3/9-char names) | `tests/aprs_extras.rs::item_rejections` |
| Mic-E encode | `tests/mic_e.rs::spec_vector_encodes` (ch. 10 destination+info bytes), `src/aprs/mic_e.rs::dest_char_table` | `tests/mic_e.rs::spec_vector_decodes` (paired direction) | `tests/mic_e.rs::round_trip_sweep` (seeded LCG sweep) | `tests/mic_e.rs::speed_course_boundaries`, `ambiguity_levels`, `hemispheres_and_offset`, `src/aprs/mic_e.rs::split_dmh_known_answers` | `tests/mic_e.rs::encode_errors`, `src/aprs/mic_e.rs::symbol_table_check` |
| Mic-E decode | `tests/mic_e.rs::spec_vector_encodes` (pinned wire input) | `tests/mic_e.rs::spec_vector_decodes`, `south_east_custom_vector`, `old_fix_type_byte` | `tests/mic_e.rs::round_trip_sweep`, `message_types_exhaustive` | `tests/mic_e.rs::altitude_and_status`, `src/aprs/mic_e.rs::altitude_splitter`, `dest_col_inverts_dest_char`, `message_bits_round_trip`, `tests/mic_e.rs::declared_ambiguity_reaches_the_coordinates` (all five levels through encode→decode; fixed defect: `MicE::coordinates()` hard-coded `Ambiguity::EXACT`, and blanked digits decode as zero, so a position blurred on purpose was reported as exact and the position alone could not reveal it), `declared_ambiguity_survives_a_real_wire_payload` (decode side, on wire bytes the reference encoder produced), `spec_chapter_10_ignores_the_low_longitude_digits` (the spec's own worked example: destination `T4SQZZ` and longitude bytes `(_f` must report 112 deg 7 min, where the crate answered 112 deg 7.74 min, 1373 m over-precise) and `tests/cli.rs::mic_e_ambiguity_is_honoured_by_both_renderers`, `out_of_range_ambiguity_saturates_at_four_digits` (a public `u8` field can hold a count the wire has no room for; saturating errs toward less claimed precision, and the value still never reaches the wire), `coordinates_stays_const_callable` (pins the `const`-ness the fix had to preserve), `spec_three_spellings_of_one_altitude` (chapter 10's `"4T}`, `>"4T}`, `]"4T}`: one altitude, three spellings), `spec_maidenhead_status_is_a_prefix_with_no_altitude` (chapter 10's `>IO91SX/G Helloworld`: a prefix with **no** altitude behind it, which is what gating the two together got wrong), `prefix_without_altitude_round_trips_byte_exactly` (all four prefixes, with and without status text), `unprefixed_altitude_outranks_a_prefix_shaped_first_digit` (39 686 m encodes to `'!!}`; without the fallback this crate's own encoder stops being invertible above 39 km), `corpus_kenwood_spells_its_prefix_both_ways` (two real frames from `AE6GR-7`, one radio in one session, spelling the same device `]"6[}` and `]Stopped`, which is the evidence that refuted the device-*suffix* hypothesis), `corpus_bare_prefix_frames`, `src/aprs/mic_e.rs::device_prefixed_altitude_round_trips_byte_exactly` | `tests/mic_e.rs::decode_errors`, `aprs_parse_rejects_mic_e_ids`, `status_beginning_with_a_prefix_byte_is_the_bounded_cost` (what the guess costs: status text that itself begins with `>` loses exactly one byte to `device_prefix`, and the *wire* is still a fixed point), `src/aprs/mic_e.rs::encode_rejects_an_invented_device_prefix` |
| APRS symbol from the AX.25 address (ch. 8 & 20: `GPSxyz` / `SPCxyz` / `SYMxyz` / `GPSCnn` / `GPSEnn`, and the source SSID) | — receive-only: the crate never *writes* a symbol into an address, so the counterpart vector is `tests/symbol_from_address.rs::source_ssid_matches_the_named_constants`, which resolves all 16 SSID rows to the crate's own named `Symbol` constants rather than to a copy of the same table | `tests/symbol_from_address.rs::chapter_20_scouts_sentence` (the spec sentence verbatim: `GPSBM`/`SPCBM`/`SYMBM`/`GPSC12` all draw Boy Scouts, `GPSOM`/`SPCOM`/`SYMOM`/`GPSE12` all draw Girl Scouts), `chapter_20_car_sentence_and_its_typo`, `chapter_20_overlay_sentence`, `chapter_20_source_ssid_table`, `chapter_20_precedence_example`, `corpus_destinations` (`GPSLJ` jeep, `GPSLK` truck, `GPSMV` car; the real off-air spellings) | `tests/symbol_from_address.rs::totality_over_every_charted_symbol` is the load-bearing one. The mnemonics are decoded **arithmetically**, as seven contiguous runs per table with disjoint leading letters, so the published 188-row chart is the independent oracle: `PRIMARY_XY`/`ALTERNATE_XY` are Appendix 2 transcribed row by row in a shape that shares no code with the implementation, and the law drives **1786** spellings through it (564 bare mnemonics, the same 564 space-filled, 188 numeric `GPSCnn`/`GPSEnn`, 376 overlays, 94 overlay rejections), with a `MIN_CASES = 1750` floor so the sweep cannot narrow to nothing and stay green. `every_charted_mnemonic_is_distinct` pins the injectivity the arithmetic relies on (188 distinct `xy`, leading letters disjoint by table) | `tests/symbol_from_address.rs::numeric_spelling_edges` (`01`–`94` accepted, `00` and `95`–`99` not, `GPS` prefix only), `illegal_overlay_characters_are_not_guessed`, `precedence_is_total_over_the_eight_combinations` (information field → destination → source SSID, all eight presence combinations), `usable_in_const_context` | `tests/symbol_from_address.rs::no_uncharted_mnemonic_decodes` covers the expensive direction, because a false positive here does not error; it draws a *plausible wrong icon*. Exhaustive over the printable `xy` square × three prefixes: **27 075** cases, of which exactly 564 may decode and **26 511** must return `None`, so a run endpoint off by one fails on the boundary row instead of passing quietly. `ordinary_destinations_name_no_symbol` covers the ordinary tocalls (`APRS`, `APU25N`, …) |
| APRS frame-level decode (`Decoded`, `decode_frame`) | — a total decoder over received bytes; the paired direction is `tests/decoded_laws.rs::receive_only_formats_survive_the_full_radio_stack`, which puts pinned NMEA / Ultimeter / third-party payloads through modulate → demodulate → decode | `tests/mic_e.rs::decoded_needs_the_destination_and_decode_frame_supplies_it` (what each of the three calls says: `AprsPacket::parse` rejects the Mic-E identifier, `Decoded::decode` answers `NeedsDestination`, `Decoded::decode_frame` answers `MicE`), `tests/decoded_laws.rs::receive_only_formats_survive_the_full_radio_stack` (the NMEA leg additionally checks the recovered coordinates) | `tests/decoded_laws.rs::frame_laws_hold_over_the_identifier_destination_cross_product` sweeps every data type identifier × twelve adversarial bodies × 42 fixed destinations (14 callsigns spanning the structure Mic-E cares about × SSIDs 0/7/15), asserting `decode_frame(d, info).info == info` and, the reason the two constructors are allowed to coexist, **destination independence**: for any information field that is not Mic-E, `decode_frame(d, info).kind == decode(info).kind` for *every* `d` | `tests/decoded_laws.rs::frame_laws_hold_for_every_prefix_of_valid_packets`, `frame_laws_hold_for_structured_random_input` (60 000 seeded cases with a random destination *and* random bytes, because on the air both halves come from a stranger), `frame_laws_hold_for_uniform_random_input` | `tests/decoded_laws.rs::mic_e_decode_never_overlaps_the_information_field_decoder` (Mic-E arrives only from `decode_frame` and only for `` ` `` / `'`; without a destination the answer is `NeedsDestination`, never the untrue `Unsupported`), and totality itself: `decode_frame` returns no `Result`, so the only failure available to it is a panic |
| G3RUH scrambler | `tests/g3ruh.rs::tx_pipeline_order_is_nrzi_then_scramble_then_synthesis` (pinned stage order), `src/scrambler.rs::all_zeros_input_is_pure_lfsr_sequence`, `tests/edge_cases.rs::g3ruh_pn_sequence` (PN first-48-bits derived from the published polynomial; output period 2^17−1) | `src/scrambler.rs::descrambler_self_synchronizes_within_17_bits`, `single_channel_error_corrupts_exactly_offsets_0_12_17` | `tests/g3ruh.rs::round_trip_100_frames_9600_baud_44100`, `round_trip_20_frames_9600_baud_48000`, `src/scrambler.rs::roundtrip_pseudo_random_sequences`, `tests/roundtrip_laws.rs::law_scrambler_roundtrip_identity` | `src/scrambler.rs::lfsr_sequence_has_maximal_period`, `all_zeros_state_and_input_stay_zero`, `tests/g3ruh.rs::profile_selects_baseband_scheme` | `tests/g3ruh.rs::baseband_receiver_stats_count_frames` (FCS/oversize tallied, never panicking; the scrambler itself is a total code) |
| FX.25 framing | `tests/fx25.rs::wrap_layout_is_tag_data_parity` (exact tag ‖ data ‖ parity layout), `byte_bits_is_lsb_first` | `tests/fx25.rs::tag_constants_are_pairwise_distance_32`, `rs_parity_matches_tag_family` | `tests/fx25.rs::fx25_modem_round_trip_multiple_frames`, `every_tag_round_trips_at_byte_level`, `backward_compat_plain_receiver_decodes_fx25_audio`, `tests/roundtrip_laws.rs::law_fx25_roundtrip_identity` | `tests/fx25.rs::smallest_tag_selection_covers_all_sizes`, `tag_hunter_tolerates_tag_bit_errors`, `plain_audio_decodes_through_fx25_receiver`, `tests/edge_cases.rs::fx25_tag_corruption` (all 64 single-bit flips, at-tolerance multi-bit, beyond-tolerance no-mislock) | `tests/fx25.rs::wrap_rejects_oversize_and_short_buffers`, `tag_hunter_rejects_beyond_tolerance`, `fx25_flags_uncorrectable_block` |
| RS(255,k) codec | `tests/rs.rs::known_answer_parity_matches_first_principles_division` (parity vs independent polynomial division), `tests/edge_cases.rs::rs_gf256_published_identities` (published GF(256) 0x11D identities: element order 255, antilog spot values, generator roots a^1..a^p, parity = long-division remainder, codeword root property; all of it via an independent shift-and-reduce GF) | `tests/rs.rs::corrects_up_to_t_random_errors` (decode restores pinned data) | `tests/rs.rs::clean_round_trip_all_parities` (parity 16/32/64), `shortened_round_trips_at_various_lengths` | `tests/rs.rs::shortened_round_trips_at_various_lengths` (block-size boundaries), `beyond_t_errors_fail_or_miscorrect_without_panic` | `tests/rs.rs::typed_errors_on_bad_slice_lengths`, `beyond_t_errors_fail_or_miscorrect_without_panic` |
| TNC TX | `tests/coverage_fill.rs::tnc_tx::build_frame_known_bytes` (frame bytes byte-exact vs the AX.25 layer) | `tests/tnc.rs::loop_status` (transmitted samples decode to the exact packet) | `tests/tnc.rs::loop_position_uncompressed` … `loop_item` (8 packet kinds × 2 rates × 2 PCM paths), `alloc_render_matches_lazy_iterators` | `tests/tnc.rs::raw_info_frame_round_trips`, `multi_frame_back_to_back_decode` | `tests/coverage_fill.rs::tnc_tx::transmit_rejects_small_buffer` (typed `TncError` on both buffers) |
| TNC RX | `tests/coverage_fill.rs::tnc_tx::build_frame_known_bytes` (pinned frame input) | `tests/tnc.rs::loop_message_with_ack_id` (decoded packet equality), `mic_e_loop` | `tests/tnc.rs::multi_frame_back_to_back_decode` | `tests/tnc.rs::truncated_stream_yields_no_false_frame` | `tests/tnc.rs::bad_fcs_counts_fcs_error`, `garbage_frame_counts_malformed` |
| CLI encode | `tests/cli.rs::encode_position_decode_round_trip` (known args → decodable WAV with pinned field text) | `tests/cli.rs::encode_message_decode_round_trip` (decoder output text checked) | `tests/cli.rs::rate_variants_round_trip` | `tests/cli.rs::rate_variants_round_trip` (8000/11025/48000 Hz boundaries) | `tests/cli.rs::bad_usage_exits_nonzero` (usage exit 2 / value exit 1) |
| CLI decode | `tests/cli.rs::decode_wav_written_by_library` (library-written WAV, pinned stdout fields) | `tests/cli.rs::decode_wav_written_by_library` | `tests/cli.rs::encode_position_decode_round_trip` | `tests/cli.rs::rate_variants_round_trip` | `tests/cli.rs::decode_bad_inputs_exit_nonzero` |
| APRS capabilities (`<`) | `src/aprs/capabilities.rs::round_trips` (exact `<IGATE,MSG_CNT=13` bytes) | `src/aprs/capabilities.rs::parses_an_igate_report` (bare flags vs `key=value`, token census) | `src/aprs/capabilities.rs::round_trips`, `tests/fuzz_decode.rs::fuzz_corrupted_valid_encodings` (in the corpus, so every corrupted parse must reach a re-encode fixed point) | `src/aprs/capabilities.rs::empty_body_and_empty_tokens` (bare `<`, and `<,,A,,`), `distinguishes_absent_from_empty_value` (`<K=` is an empty value, not an absent one) | `src/aprs/capabilities.rs::rejects_the_wrong_identifier` (typed `InvalidDataType` / `Truncated`), `tests/fuzz_decode.rs::fuzz_aprs_subparsers_random` (`Capabilities::parse` on random and DTI-prefixed input: totality, not correctness) |
| `Discriminator` extension seam | `tests/coverage_fill.rs::caller_supplied_discriminator::third_party_front_end_decodes_through_with_discriminator` (the payload bits are pinned; a delay-and-multiply front end written in the *test* crate must recover them) | same test (its i16 and f32 halves both decode the same pinned payload) | same test: the caller's front end is held to the built-in correlator's result on the same audio, with at most one bit of startup drift | same test: `Demodulator::with_discriminator` is the crate's only public door to the `Discriminator` trait, so implementing it from a test crate is also the proof that only public items are needed | same test's negative control: an `AlwaysMark` front end whose metric never changes sign must slice to all ones and must *not* produce the payload; without it, a constructor that quietly built its own correlator would pass |

## Not yet in this matrix

The matrix covers the layers that existed when it was written. These
ship, are tested, and still have no row: IL2P (`tests/il2p.rs`,
`tests/il2p_audio.rs`), WSPR (`tests/wspr.rs`, `tests/wspr_rx.rs`), FT8
(`tests/ft8.rs`, `tests/ft8_rx.rs`), M17 (`tests/m17.rs`), the
crate-root position primitives (`tests/geo.rs`) and physical quantities
(`tests/units.rs`), the receive-only APRS formats (NMEA, Ultimeter and
third-party, which have in-module tests only), the `digipeat` relay
core (proven through `tests/app_examples.rs` and
`tests/esp32_examples.rs`), the async and embassy adapters
(`tests/asynk.rs`, `tests/embassy.rs`), the `SampleRing`
bounded-latency intake (`tests/bounded_latency.rs`), the demodulator
normalization and bit-exactness suites
(`tests/demod_normalization.rs`, `tests/equivalence.rs`), the KISS
server transport (`tests/serve.rs`), the HDLC frame-length edges
(`tests/hdlc_edge.rs`), and the public type-size ratchet
(`tests/type_sizes.rs`). MEASURED against the 51 files under `tests/`:
**31 are cited nowhere in the matrix**. That count is the number of
owed rows, and it is the number to watch, because the sections below
can and do cite a file without giving its layer a row. When
`tests/symbol_from_address.rs` landed the count fell by two instead of
rising by one, because that file arrived *with* its row and
`tests/decoded_laws.rs` finally got one. Ten are cited nowhere in this
document at all except this paragraph:
`tests/demod_normalization.rs`, `tests/equivalence.rs`,
`tests/geo.rs`, `tests/hdlc_edge.rs`, `tests/il2p.rs`,
`tests/il2p_audio.rs`, `tests/m17.rs`, `tests/serve.rs`,
`tests/type_sizes.rs` and `tests/units.rs`.

One correction that is *not* a removal: the IL2P, M17, WSPR and FT8
entries above each gained a pinned public function in "Public functions
that no test, doctest or example called" below
(`Il2pParity::baseline_for_block`, `PacketAssembler::lsf`,
`WsprModulator::fill_f32`, `Ft8Modulator::fill_f32`). One function is
not a row, so they stay on this list.

Three entries have come off this list since it was written, and only
these three. **Capabilities** now has a matrix row of its own: `<` is in
the DTI sweep, `Capabilities::parse` is fuzzed, and a capabilities
encoding is in the truncation/corruption corpus. The total `Decoded`
decode path (`tests/decoded_laws.rs`) had a weaker claim recorded here:
cited in the fuzz table below, but with no row. It now has the row,
because `Decoded::decode_frame` gave the layer a public entry point of
its own and four laws worth tabulating. **Address-borne symbols**
(`tests/symbol_from_address.rs`) is the first file in a while to arrive
with its row already written instead of owing one.

The same omission was recorded and closed once for G3RUH/FX.25/RS, and
it has recurred with the layers added since. Those rows are still owed.

## Cross-cutting suites

- **Runtime no-alloc proof.** `tests/no_alloc.rs` installs a counting
  `#[global_allocator]` (a thread-local counter around the system
  allocator) and proves ZERO heap allocations inside the measured
  windows of (a) AX.25 UI build → TNC modulate to i16 → demodulate →
  frame recovery, (b) FX.25 stuff + RS wrap → bit-level receive, and
  (c) KISS encode → deframe. Setup and assertions run outside the
  windows.
- **Per-layer roundtrip laws.** `tests/roundtrip_laws.rs` states
  explicit `decode(encode(x)) == x` laws over 300 LCG-seeded cases per
  layer (NRZI, G3RUH scrambler, HDLC stuffing+FCS, AX.25 UI frames,
  FX.25 wrap/receive, KISS, APRS position uncompressed-exact /
  compressed-within-quantization, and the composed NRZI∘scrambler
  stack). Fixed literal seeds, no new dev-dependencies. KISS port 12
  is excluded with an in-test derivation: its Data command byte equals
  FEND, a wire ambiguity inherent to classic KISS framing.
- **Published known answers + capacity edges.** `tests/edge_cases.rs`
  (see matrix rows above) holds CRC RevEng catalogue vectors,
  GF(256)/RS identities from the FX.25-published parameters, the G3RUH
  PN sequence derived from the published polynomial, HDLC deframer
  capacity/capacity+1 edges, all-ones stuffing stress, and byte-level
  FX.25 tag-corruption resolution/rejection.
- **Benchmark pin.** `tests/benchmark.rs::synthetic_noise_row_never_regresses`
  pins the synthetic-noise row at ≥ 74 (its measured current value) on
  the operator-provided fixed noise WAV; the four real-world pins
  (999/985/100-exact/98) are unchanged. Two further synthetic pins
  landed later in the same file:
  `synthetic_noise_fx25_row_never_regresses` (≥ 92) and
  `synthetic_noise_300_baud_row_never_regresses` (≥ 74).

### Public functions that no test, doctest or example called

A mechanical sweep of the crate's public functions produced this list;
none of it came from reading the source. An earlier round of that sweep
produced `tests/coverage_fill.rs::untested_public_builders_round_trip`
(eleven uncalled builders, of which two had been added the same day)
and `untested_public_accessors`. Seven more were still unreached. Each
now has a test that asserts a *result*, since a call with no assertion
behind it checks nothing.

| Function | Test (all in `tests/coverage_fill.rs`) | What it asserts, and what it does not |
|---|---|---|
| `Demodulator::with_discriminator` | `caller_supplied_discriminator::third_party_front_end_decodes_through_with_discriminator` | The advertised PHY extension point, walked through from outside `src/` for the first time: a delay-and-multiply front end implemented in the test crate recovers the pinned payload on both the i16 and f32 halves of the trait, and an `AlwaysMark` negative control proves the constructor consults the caller's object instead of quietly building its own correlator |
| `TncConfig::with_flags` | `tnc_with_flags_distinguishes_preamble_from_tail` | Two same-typed positional parameters are a transposition hazard, and the total sample count is symmetric in them, so a count-based test cannot catch the swap. Pinned structurally instead: raising the tail count appends (the shorter stream stays a prefix of the longer), raising the preamble count does not |
| `WsprModulator::fill_f32` | `wspr_fill_f32_tracks_fill_i16_and_fills_what_it_claims` | The f32 path tracks the exercised i16 path sample for sample within one sine-table step, every sample stays inside `-1.0..=1.0`, the whole 162 × 8192-sample transmission is emitted, and an out-of-range sentinel proves nothing past the returned count is written. A finished modulator fills nothing |
| `Ft8Modulator::fill_f32` | `ft8_fill_f32_tracks_fill_i16_and_fills_what_it_claims` | The same two claims over the GFSK-shaped 8-FSK generator, 79 × 1920 samples |
| `PacketAssembler::lsf` | `m17_packet_assembler_reports_the_link_setup_frame` | Asked about an LSF that came back **off the air**: transmit, recover through FEC and CRC, assemble the payload, *then* read the accessor. Both callsigns in the right slots (a transposed dst/src would be invisible against a symmetric LSF) and every field of the 16-bit TYPE word; `None` before any LSF, and a second `start` replaces it, so the accessor reports live state |
| `Il2pParity::baseline_for_block` | `il2p_baseline_parity_table_is_pinned_across_the_block_size_domain` | Pins a wire-compatibility decision, not a doc comment: IL2P draft v0.4 prints a table *and* a formula that disagree, and the formula names odd parity lengths `Il2pParity` does not have. Both edges of all four table rows plus the degenerate and saturating ends, six block sizes where the formula is undefined, domain-wide monotonicity, and agreement with what the on-air arithmetic spends |
| `Fx25Frame::is_empty` | `fx25_frame_is_empty_agrees_with_len_over_the_whole_domain` | Returns a constant `false`, and **that is the whole constructible domain**: no caller can observe a `true` case, so no test can assert one. The strongest statement available is the equivalence `is_empty() == (len() == 0)`, asserted over all eleven correlation tags and both selection paths, with an assertion that at least four *distinct* values of `len()` were involved. This catches `len()` learning to return 0 while `is_empty()` keeps saying `false` |

Validated-constructor doctests (three audiences: common one-liner,
fully typed, raw wire hatch) additionally cover construction-time
validation for `Position` (`src/aprs/position.rs`), `Object`/`Item` and
`Timestamp::dhm_zulu` (`src/aprs/object.rs`), `MicE::new` + `with_*`
(`src/aprs/mic_e.rs`) and
`PositionWeather::new`/`PositionlessWeather::new`
(`src/aprs/weather.rs`); they run as part of `cargo test`'s doctest
pass. `Timestamp::dhm_local` and `hms` have no doctest of their own.
They are covered instead by `src/aprs/object.rs`'s in-module tests and
by `tests/compressed.rs::timestamped_positions_round_trip_both_dtis`,
outside the three-audience pattern. `PositionlessWeather` has two
audiences, the common path and the raw hatch, rather than three.

### What the ignored doctests do and do not catch

MEASURED: the doctest pass reports 159 doctests, 150 run and 9
ignored, and all 9 ignored ones are ```` ```rust,ignore ```` fences in
prose files, 6 in `README.md` and 3 in `docs/EMBEDDED.md`, with none
anywhere else that rustdoc collects. Both files reach rustdoc through
`#[doc = include_str!(...)]` on the `ReadmeDoctests` and
`EmbeddedDoctests` markers in `src/lib.rs`. The
count was 151 until an ```` ```ignore ```` fence in `src/il2p.rs`,
whose second line is intentionally invalid illustrative code, became
```` ```text ````. It had to: `ignore` does not mean "skip this", it
means "a doctest that only runs under `--ignored`", so
`cargo test -- --ignored` failed to compile it for everyone, with or
without any external material.

The non-obvious part, and the reason this belongs in a coverage record:
**a `rust,ignore` fence is only ever *parsed*. Neither run mode
type-checks it, compiles it or runs it.** So an API rename inside one
of these fences is caught by nothing at all.

VERIFIED with an isolated two-fence crate outside this repo, on this
crate's edition (2024, so rustdoc's merged-doctest runner), which
separates the two error classes that are easy to conflate:

| Fence body | default `cargo test --doc` | `cargo test --doc -- --ignored` |
|---|---|---|
| `let x = ( ;` (syntax error) | `ignored` | **FAILED**, `unclosed delimiter` |
| `ThisTypeDoesNotExistAnywhere::bogus(42)` (valid syntax, unresolvable name) | `ignored` | **passes** |

The same probe run against `README.md` itself agrees: injecting a
nonexistent type into one of the ten fences leaves the `--ignored` run
at 10 passed, while injecting a stray token fails it.

Under `--ignored`, rustdoc must parse each fence to build a harness
entry. That is why a *syntax* error surfaces, and how the `src/il2p.rs`
fence above went unnoticed until somebody ran `--ignored`. Parsing is
as far as it goes: rustdoc resolves no names, so the fences cannot
drift-detect the API they illustrate. `.github/workflows/ci.yml` runs
plain `cargo test --all-features`, so CI does not even reach the parse.

What catches drift instead is live coverage of the same API elsewhere,
which most of the fences have:

| README fence | Live coverage of the same API |
|---|---|
| `asynk::decode_wav`, `decode_many`, `frames`, `decode_stream` (4 fences) | `tests/asynk.rs` (13 tests) |
| `embassy::run_decoder` / `TxTicker` | `tests/embassy.rs`, `examples/balloon_tracker_embassy.rs` |
| `SampleRing` + `TncConfig::bounded_latency()` superloop | `tests/bounded_latency.rs`, `tests/embassy.rs` |
| `digipeat::relay_decision` / `DupeRing` | `tests/digipeat_laws.rs`, `tests/app_examples.rs`, `tests/esp32_examples.rs`, `examples/digipeater_station.rs` |
| `wav::sniff_pcm` / `decode_sniffed` thread sketch | the CLI itself (`src/bin/yodel/shared.rs`, driven by `tests/cli.rs`) and `examples/balloon_tracker.rs` |
| WSPR transmit + decode round trip | `tests/wspr.rs`, `tests/wspr_rx.rs`, `examples/wspr_beacon.rs` |
| FT8 transmit + decode round trip | `tests/ft8.rs`, `tests/ft8_rx.rs`, `examples/ft8_cycle.rs` |

That covers all ten, and `cargo test` does build the examples, so a
rename breaks something CI runs. The coverage is *only* indirect,
though. Nothing checks the fences themselves, so a fence that drifts
from the API it illustrates stays green everywhere, including under
`--ignored`, and the table above is all that stands behind them.

## Differential vs reference

Measured with `tests/differential.rs` (plus the older bidirectional
harness in `tests/oracle.rs`), both `#[ignore]`-gated behind the
`YODEL_REF_GEN` / `YODEL_REF_DECODE` environment variables naming the
external reference generator/decoder binaries:

```
YODEL_REF_GEN=… YODEL_REF_DECODE=… \
  cargo test --all-features --test differential -- --ignored --nocapture
```

Everything is deterministic: a seeded LCG generates the corpus and the
channel noise; no time or external randomness enters the tests.

RE-VERIFIED with the reference binaries present: all five ignored tests
in `tests/differential.rs` pass, and both tables below reproduce
exactly, at 320/320 in each direction with the per-kind census as
printed and 50/50 for both decoders at every rung of the shootout. The
untabulated legs report 100/100 each: `differential_300_baud` (b) and
(c), `differential_9600_baud` (b) and (c), and `differential_fx25` (a),
(b) and the additive plain-receiver leg.

The two legs tabulated below are the 1200-baud ones.
`tests/differential.rs` also carries `differential_fx25`,
`differential_300_baud` and `differential_9600_baud` (five ignored
tests in that file), and four further differential files ship
untabulated here: `tests/aprs_differential.rs` (field-by-field APRS
comparison, 2), `tests/il2p_differential.rs` (2),
`tests/wspr_differential.rs` (3) and `tests/ft8_differential.rs` (3).
`tests/corpus_aprs.rs` (2) compares against the operator-provided
`corpus/` recordings.

### Tier-3/4 guard hardening

The most expensive way a test can fail is to pass while testing
nothing, and every suite in this section is `#[ignore]`d, so nobody
watches it. Four such holes were found and closed; they are recorded
here because the fix is invisible in a green run.

| Suite | What it did | What it does now |
|---|---|---|
| `tests/oracle.rs` | Seventeen of its 31 ignored tests **failed** rather than skipped when `YODEL_REF_GEN`/`YODEL_REF_DECODE` were unset: they called a helper that panics on an unset variable. The skip guard existed, but only in the newer half of the file | One `ref_binaries_available` shared by both halves, so they cannot drift apart again. All 31 skip cleanly. VERIFIED: `cargo test --all-features -- --ignored` with the variables unset is green |
| `tests/oracle.rs`, `tests/differential.rs` | Tested `is_none()` before validating, so `YODEL_REF_GEN=/typo` with `YODEL_REF_DECODE` unset skipped **in silence**, so a typo could turn an entire interoperability suite green | Both variables are resolved before the skip decision. Unset skips; set-but-wrong fails loudly, with a message saying to unset it if the skip was intended. VERIFIED on exactly that combination: all five `differential` tests and every `oracle` test that needs a binary now fail with the path in the message |
| `tests/aprs_differential.rs` | Read `YODEL_REF_APRS` with no existence check at all. A bad path failed minutes later inside `spawn`, and when `corpus/` happened to be absent it did *not* fail at all, because that check came first and returned a silent skip | Same `ref_binary` rule, checked up front and independent of what other material is present; canonicalized, because the decoder is run beside its own data files. VERIFIED: `YODEL_REF_APRS=/nope/typo` fails both tests immediately |
| `tests/ber.rs` | Every assertion lives inside a loop, and a ceiling of `None` asserts nothing, so a ladder trimmed to one rung, or one with every ceiling replaced by `None`, measured and printed exactly as much as a pinned one | `MIN_RUNGS` (6 per ladder), `MIN_PINNED_CEILINGS` (8, the smallest of the three ladders' MEASURED 12/14/8), `MIN_MODES` (2, not 3, because the third is behind the `g3ruh` feature), and a floor on the bit centres the perfect-clock column compares |
| `tests/differential.rs` | The shootout ladder asserts `ours >= reference`, which `0 >= 0` satisfies: a transmitter emitting silence, or a reference binary decoding nothing, would sweep every rung | `MIN_CLEAN_RECOVERED` (= 50 for both decoders on the noise-free rung, which is a correctness property, since that audio is our own unmodified transmission) and `MIN_CORPUS_CASES` (300 of the 320 generated) |

The same class of guard now sits in `tests/fuzz_decode.rs` and
`tests/wspr_differential.rs`; see below and the fuzz table.

### The WSPR differential suite had never executed

`tests/wspr_differential.rs::channel_symbols_match_the_reference_encoder`
is the leg that compares this crate's composed WSPR encoding against an
independent implementation, which is the whole reason the file exists.
Two bugs made a panic its only possible outcome. It passed one fixed
argument form, so against an encoder that takes no flag it handed the
flag over *as the message*; and it then searched for all 162 symbols on
a single line, so against an encoder that wraps them it could only
panic. It now tries both known argument forms in a fixed order and
reads tokens after the `Channel symbols:` header until it has 162,
wrapped or not.

MEASURED with the reference present, and re-run for this document:
**5/5 on all three legs**, against a `MIN_CASES = 5` floor.
`channel_symbols_match_the_reference_encoder` reports 5/5 messages
identical, `our_transmission_decodes_in_the_reference_decoder` 5/5
recovered, and `we_decode_the_reference_transmission` 5/5 recovered.
The floor is the same idiom used above, because all three legs'
assertions are inside a loop over the case list. The three
`YODEL_REF_WSPR_*` variables are resolved through the same
unset-skips / set-but-wrong-fails rule as above.

One caveat that can cost an hour to rediscover: the WAV-writing
generator this suite wants is the floating-point build.
`YODEL_REF_WSPR_GEN` pointed at the integer sibling exits non-zero
with an empty stderr, and the suite then fails with
`reference generator failed:` and nothing after the colon. The guard is
working there, since a set-but-wrong path is refusing to skip, but the
message does not say which binary to reach for.

### Corpus and agreement (`differential_corpus`)

320 packets (20 per kind, 16 kinds), rotating callsigns/SSIDs
(`N0CALL`…`N9CALL`, SSID 0–15), digipeater paths (none / `WIDE1-1` /
`WIDE1-1,WIDE2-1`), all four lat/lon quadrants, all `Timestamp` forms
(DHM zulu, DHM local, HMS), Mic-E message codes cycling through all 15
and ambiguity cycling 0–4:

| kind | cases | (a) ours→ours | (b) ours→reference | (c) reference→ours |
|---|---|---|---|---|
| pos_uncompressed | 20 | 20/20 | 20/20 | 20/20 |
| pos_uncompressed_msg | 20 | 20/20 | 20/20 | 20/20 |
| pos_compressed_nodata | 20 | 20/20 | 20/20 | 20/20 |
| pos_cs_course_speed | 20 | 20/20 | 20/20 | 20/20 |
| pos_cs_radio_range | 20 | 20/20 | 20/20 | 20/20 |
| pos_cs_altitude | 20 | 20/20 | 20/20 | 20/20 |
| pos_ts_uncompressed | 20 | 20/20 | 20/20 | 20/20 |
| pos_ts_compressed | 20 | 20/20 | 20/20 | 20/20 |
| message (text/id/ack/rej) | 20 | 20/20 | 20/20 | 20/20 |
| status | 20 | 20/20 | 20/20 | 20/20 |
| wx_positionless | 20 | 20/20 | 20/20 | 20/20 |
| wx_position | 20 | 20/20 | 20/20 | 20/20 |
| telemetry | 20 | 20/20 | 20/20 | 20/20 |
| object (live/killed, both DHM + HMS) | 20 | 20/20 | 20/20 | 20/20 |
| item (live/killed) | 20 | 20/20 | 20/20 | 20/20 |
| mic_e (4 quadrants, 15 codes, ambiguity 0–4) | 20 | 20/20 | 20/20 | 20/20 |
| **total** | **320** | **320/320** | **320/320** | **320/320** |

Directions: (a) our typed encode → our typed decode returns equal
values; (b) our TNC transmit → WAV → reference decoder reports our
source, destination, path and info byte-for-byte; (c) our monitor text
→ reference generator → WAV → our `TncReceiver` recovers the identical
frame (and its info still parses with our typed decoders). Agreement is
100 % on all three axes.

### SNR shootout (`snr_shootout`)

The first 50 corpus frames, modulated once by our transmitter at
44.1 kHz, with seeded additive uniform white noise applied per level;
the *same* WAV is decoded by our `TncReceiver` and by the reference
decoder, counting complete frame recoveries (ours matched byte-exactly
against the transmitted frame bodies; the reference by its own decode
count):

| SNR | ours | reference |
|---|---|---|
| clean | 50 | 50 |
| 10 dB | 50 | 50 |
| 5 dB | 50 | 50 |
| 3 dB | 50 | 50 |
| 2 dB | 50 | 50 |
| 1.5 dB | 50 | 50 |

The test asserts `ours >= reference` at every level; we tie at 50/50
everywhere down to 1.5 dB, so no demodulator changes were needed.
Below the asserted ladder both decoders leave their clean-decode
region and counts fall steeply; measured once for reference (not
asserted, seeds as in the test): at 1 dB ours 46 vs reference 49, at
0 dB ours 35 vs reference 49. The reference decoder holds on longer in
the deeply-degraded region, where it runs multiple parallel
demodulation profiles. That is a known headroom gap outside the
asserted ladder, and not a regression at any asserted level.

### Legitimate differences / normalizations

- **Monitor-line escaping.** The reference decoder prints control
  bytes in the info field as `<0xNN>` (lowercase hex), and the
  reference generator carries the frame file's trailing newline into
  the info field. The harness escapes expected info bytes with the
  same rule and expects a trailing `<0x0a>` / `0x0A` where the
  generator appended one.
- **`csT` canonicalization.** The compressed course/speed, radio
  range and altitude wire codes are exponential (`1.08^n`, `1.002^n`),
  so an arbitrary input value is first canonicalized to its nearest
  representable wire form by iterating our build→parse to a fixed
  point. That iteration now settles in one round for every scale:
  `build` inverts the decoder rather than the power, so a value read
  off the wire writes back to a code that reads as the same value.
  Several codes still decode alike, so the canonical bytes need not be
  the received ones; they round-trip exactly and agree with the
  reference byte-for-byte.
- **Mic-E corpus restricted to printable info bytes.** The reference
  generator synthesizes audio from monitor *text*, so a Mic-E info
  field containing control/DEL bytes cannot be fed through direction
  (c). The corpus constrains longitude degrees (avoiding the
  `d+28 = 0x7F` code), hundredths, speed (≤189 kn) and course so every
  info byte stays in `0x20..=0x7E`. Non-printable Mic-E wire bytes are
  covered separately by `tests/mic_e.rs` (typed round trips) and by
  the raw-frame direction-A oracle tests.
- **SSID 0 rendering.** Both sides print SSID-0 addresses without a
  `-0` suffix; the corpus renders headers the same way.

## Robustness

Fuzz-style decode tests (`tests/fuzz_decode.rs`) and a pinned
seeded-noise SNR ladder (`tests/snr.rs`). Everything is deterministic
(fixed-seed 64-bit LCG, Knuth MMIX constants), runs in the normal
`cargo test` pass (not ignored), and completes in well under a second
per file.

### Fuzz families (0 panics across all of them)

Every row below asserts **totality**: no input reaches a panic.
Correctness is not claimed, except where a row says otherwise (the
corruption row's re-encode fixed point is a correctness claim). These
rows report 0 panics and nothing beyond that; a typed error counts as
a pass.

The first two rows' claims used to be prose, and prose rotted: the sweep
claimed "every DTI branch" while silently missing `<`, because nothing
compared the claim to the code.
`fuzz_decode.rs::dti_table_and_corpus_cover_every_aprs_packet_variant`
now **re-derives** both claims from the parser instead of restating
them. The dispatch set comes from sweeping all 256 possible first bytes
and keeping the ones that are not `InvalidDataType`; the variant set
comes from parsing the corpus and mapping each result through a
`variant_index` whose `#[non_exhaustive]` wildcard arm is `None`.
Adding a DTI or a twelfth `AprsPacket` variant to `src/aprs.rs` fails
that test until the fuzz inputs follow. It also asserts the `DTIS`
table has no duplicate (a duplicate would inflate the case counts
below without adding a branch) and that the corpus meets a
`MIN_CORPUS_CASES` floor.

| Family | Test | Cases |
|---|---|---|
| `AprsPacket::parse`, fully random bytes | `fuzz_aprs_packet_parse_random` | 3 000 inputs |
| `AprsPacket::parse`, **all eleven** dispatched DTI branches (`!` `=` `/` `@` `:` `>` `_` `T` `;` `)` `<`) plus the two Mic-E identifiers (`` ` `` `'`), which `AprsPacket::parse` rejects but must reject *totally*, each with a random tail. The `<` capabilities branch is now among them, and the table is typed `[u8; DISPATCH_DTI_COUNT + MIC_E_DTI_COUNT]` so it cannot be shortened silently | `fuzz_aprs_packet_parse_random` | 13 × 1 050 = 13 650 inputs |
| **All thirteen** public APRS sub-parsers on random + DTI-prefixed inputs: `Position`, `PositionCs`, `PositionTimestamped`, `PositionWeather`, `PositionlessWeather`, `Telemetry`, `Object`, `Item`, `Status`, `Message`, `Capabilities`, `DataExtension` and `Timestamp`. `DataExtension::parse` is fuzzed under **two** symbols (`/>` and `/_`) because the weather symbol switches the same seven bytes from course/speed to wind, so neither branch is reached only by accident. The receive-only formats are still fuzzed through `Decoded::decode` in `tests/decoded_laws.rs` rather than here | `fuzz_aprs_subparsers_random` | 2 000 × 15 + 13 × 200 × 13 = 63 800 parse calls |
| Mic-E decode: random dest+info, plus forced length-6 dests with `` ` ``/`'` DTI printable-biased info | `fuzz_mic_e_decode_random` | 6 000 cases |
| `Decoded::decode_frame`: **both halves of the frame come from a stranger**, so the destination address is generated too. Beyond totality this carries a correctness claim: destination independence, checked on every case, so that for any non-Mic-E information field the answer must equal `Decoded::decode`'s. `mic_e_decode_never_overlaps_the_information_field_decoder` re-measures the partition that law rests on (**0** of 60 420 Mic-E successes fell on another identifier, and **0** overlapped a typed `AprsPacket`, with the count of Mic-E successes asserted non-zero so it cannot pass vacuously) | `decoded_laws.rs::frame_laws_hold_*` | 60 000 structured + 30 000 uniform + (53 DTIs × 12 bodies + 257) × 42 dests + every prefix and suffix of 7 valid packets × 42 dests |
| KISS: command parse exhaustive over all 256 bytes; deframer fed 20 000 random bytes + 200 FEND/FESC-biased streams | `fuzz_kiss_deframer_and_command` | ~40 000 bytes |
| AX.25 `UiFrame::parse` random + shifted-ASCII-address-biased; `Address::decode` on random 7-byte fields | `fuzz_ax25_frame_parse_random` | 6 000 frames + 4 000 addresses |
| HDLC deframer: 50 000 random bits + 500 flag-interleaved bursts | `fuzz_hdlc_deframer_random_bits` | ~66 000 bits |
| Truncation: every prefix `[0..len)` of twelve valid encodings, which reach **all 11** `AprsPacket` variants. `PositionCs`, `PositionTimestamped`, `PositionWeather` and `Capabilities` were the four that used to be absent, and the twelfth encoding is a second `Position` spelling (compressed). Reachability is asserted in code, not in prose: see `dti_table_and_corpus_cover_every_aprs_packet_variant` above | `fuzz_truncated_valid_encodings` | 12 encodings × all lengths |
| Corruption: 1–4 random byte replacements / bit flips per mutant over the valid corpus; successful parses must reach a byte-for-byte re-encode fixed point. The csT altitude used to be **exempt** here, because decode truncates to feet while encode rounded to the nearest `1.002^n` code, so the cycle wandered by a foot instead of settling. `build` now inverts the parser rather than the power, the exemption is gone, and the altitude trailer has joined the corpus | `fuzz_corrupted_valid_encodings` | 3 000 mutants |
| AX.25 truncation (every cut) + bit-flip corruption of an FCS-appended UI frame | `fuzz_ax25_truncation_and_corruption` | all cuts + 3 000 mutants |
| TNC receiver PCM: 60 000 random i16 samples (counters checked monotonic), 4 × 20 000 pathological rails/alternating extremes, 20 000 f32 samples incl. NaN/±∞/out-of-range | `fuzz_tnc_receiver_pcm` | 160 000 samples |

Result: **0 panics**. Every failure is a typed error (`AprsError`,
`MicEError`, `KissError`, `Ax25Error`) or a silently discarded
non-frame. No input-reachable panic was found in `src`, so no source
fixes were needed.

### Pinned SNR ladder (`tests/snr.rs`)

30 deterministic APRS frames per rung, seeded uniform white noise mixed
at the exact SNR (defined against a full-scale sine's RMS), decoded by
`TncReceiver`. Deterministic seeds make the counts exact; the pinned
minimums are the measured values.

| SNR (dB) | rate (Hz) | measured | pinned minimum |
|---------:|----------:|---------:|---------------:|
| 20 | 11 025 | 30/30 | 30 |
| 10 | 11 025 | 30/30 | 30 |
| 5 | 44 100 | 30/30 | 30 |
| 0 | 44 100 | 24/30 | 24 |
