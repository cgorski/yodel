# `ft4_ft8_public`: the FT4/FT8 protocol tables, in the public domain

This directory holds two data files vendored verbatim from the FT4/FT8
protocol resource package published by the protocol's authors:

```text
generator.dat            the 83x91 generator matrix of the LDPC(174,91) code,
                         as 83 rows of 91 ASCII binary digits (after a
                         3-line header)
parity.dat               the sparse 83x174 parity-check matrix of the same
                         code, as 174 lines of 3 one-based row indices — one
                         line per column
free_text_to_f71.f90     the 42-character free-text alphabet (its `data c/`)
std_call_to_c28.f90      the four positional character sets of the c28
                         callsign field (its `data a1/`..`a4/`)
```

The two `.f90` files are reference programs, vendored for the alphabets
they declare rather than to be compiled.

## Why they are here

`src/ft8.rs` embeds both matrices as Rust constants. Embedding a table
without a citable source is how a permissively licensed crate acquires a
licensing problem it cannot see, so the source of truth is vendored beside
the code and the embedded constants are **checked against these files by
the test suite** rather than trusted:

| Constant in `src/ft8.rs` | Verified against | Test |
|---|---|---|
| `GENERATOR_BITS` | `generator.dat` | `generator_bits_match_public_domain_file` |
| `CHECK_ROWS` | `parity.dat` | `check_rows_match_public_domain_parity_file` |
| `FREE_TEXT_ALPHABET` | `free_text_to_f71.f90` | `alphabets_match_public_domain_files` |
| `C28_SETS` | `std_call_to_c28.f90` | `alphabets_match_public_domain_files` |

(All in `tests/ft8.rs`.)

Those tests are tier 1: they need nothing but the repository, and they gate
CI. If a constant is ever edited, or one of these files is replaced, CI says
so.

`CHECK_ROWS` is *additionally* proven to be derivable from the generator
alone, by `tests/ft8.rs::check_rows_match_derivation_from_generator`. Two
independent confirmations of the same table is deliberate: one pins it to
the published source, the other pins it to the mathematics.

## Provenance

**Package:** `ft4_ft8_protocols.tgz`, distributed from the ARRL *QEX* files
page (`www.arrl.org/QEXfiles`) as the online resource accompanying:

> Steven J. Franke (K9AN), Bill Somerville (G4WJS), and Joseph H. Taylor
> (K1JT), "The FT4 and FT8 Communication Protocols", *QEX*, July/August
> 2020, pp. 7-17. Reference [14] of that paper.

The package's own top-level directory is named `ft4_ft8_public`, which this
directory mirrors. The vendored files are byte-identical to the package
contents apart from CRLF-to-LF normalization.

**Recovering the package.** The URL the paper gives for reference \[14\],
`http://physics.princeton.edu/pulsar/k1jt/ft4_ft8_protocols.tgz`, is dead
(Princeton migrated the pulsar group's pages; the host now answers
403/404), and the current WSJT-X site does not host it. It survives at:

- <https://web.archive.org/web/20220726174545id_/https://physics.princeton.edu//pulsar/k1jt/ft4_ft8_protocols.tgz>
- ARRL's *QEX* files page, <https://www.arrl.org/qexfiles>, under the
  July/August 2020 entry

That fragility is exactly why the files are vendored here rather than
cited by URL alone.

**Checksums** (SHA-256, after CRLF-to-LF normalization):

```text
9d9fc90db8b10e8d64d3e269dbc91b034331a244a1db15d2de701fcad9f7ce47  generator.dat
0f5ee875d3712dbcb20c366dbd50700364f5f0c0af07ef5f73f541e30c1536a0  parity.dat
9eb6bb75924b7574015baf8a335513ca785144c2f6ccd9818368d20df190adef  free_text_to_f71.f90
f002b93d5fe3633027ae1119aa761ce6ca0f5f3c72bd4d9a49aeda9b0a843a29  std_call_to_c28.f90
19d680a2676490a829245b90d386b72c8f2ea857fcaff3c872a7bcd9ba784f84  ft4_ft8_protocols.tgz (as retrieved)
```

The paper cites these files as the normative definition of the code, §3:

> "The generator matrix has 83 rows and 91 columns. It is defined in a file
> `generator.dat` and included along with a number of other useful files in
> reference [14]. [...] Similarly, a file `parity.dat` [14] defines the
> sparse 83 x 174 parity-check matrix."

## Licence: public domain, with conditions on the *names*

§9 of the paper, "Concluding Remarks and Software License", places the
protocol description and the reference-[14] resources in the public domain,
and explicitly carves them out of WSJT-X's GPLv3:

> "This paper and the online resources found in reference [14] provide
> complete descriptions of the FT4 and FT8 protocols. In the spirit of open
> sharing, and to encourage other software developers who might use some of
> our ideas, **we place this description in the public domain** with the
> following restrictions:
>
> - Other software implementers may use the names "FT4" and "FT8" only if
>   they adhere to our protocol definitions for source encoding,
>   error-correction coding, and modulation format.
> - Robotic or unattended QSOs must be explicitly disallowed.
> - Multi-streaming with waveforms and message content similar to those used
>   in FT8 DXpedition Mode is permissible only within the guidelines
>   specified in the WSJT-X 2.1 User Guide [19].
> - Presently unassigned message types (see Table 1) are reserved for future
>   expansion and must not be assigned without our permission.
> - Any implementation of these or similar protocols that allows robotic,
>   unattended, or non-conforming multi-streaming operation shall not use the
>   names "FT4" or "FT8" and must be made incompatible by some means, such as
>   using different Costas arrays for synchronization.
>
> **With the exception of code contained in reference [14]**, source code for
> our implementations of FT4, FT8, and MSK144 is not in the public domain.
> Rather, all code in WSJT-X is copyrighted and licensed under the terms of
> Version 3 of the GNU General Public License (GPLv3) [...]
>
> We welcome any independent software implementations of FT4 and FT8, so long
> as they either (1) adhere to all requirements mentioned above, or (2) **make
> no use of our source code beyond the public-domain resources mentioned
> above.**"

Two consequences that matter for this crate:

1. **These tables carry no copyleft obligation**, so embedding them in an
   MIT/Apache-2.0 crate is exactly what the authors invited. Route (2) above
   is the one this crate takes: the public-domain resources are used, the
   GPLv3 implementation is not.
2. **The dedication is conditional, and the conditions bind us**, because
   this crate uses the name "FT8". See the "Protocol licence and conditions"
   section of the `src/ft8.rs` module documentation for the per-condition
   compliance statement.

The quoted article text is copyright ARRL and is reproduced here as a short
excerpt for attribution and compliance purposes. The `.dat` files themselves
are the public-domain resources.

## A note on representation

`generator.dat` stores each row as **91 binary digits**. Some implementations
store the same matrix as 23 or 24 hexadecimal characters per row, which
requires padding the 91 bits out to 92 or 96. Those hex forms are
representation choices made by particular implementations, not part of the
protocol; `src/ft8.rs` uses neither, storing each row as a 91-bit integer
(`u128`) and deriving it from this file's binary rows in the test above.
See `CONTRIBUTING.md`, "The permitted exception: data the authors put in
the public domain", obligation 3, for why that distinction was worth
acting on.
