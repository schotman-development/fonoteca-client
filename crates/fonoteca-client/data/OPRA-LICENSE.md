# OPRA data notice

`opra.json.gz` is an extract of the database of OPRA
(https://github.com/opra-project/OPRA), an open, community-maintained
directory of headphone EQ compensation curves started at Roon Labs. Its
vendors, products and EQ data are licensed under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/legalcode.en),
and this extract stays under that licence.

The extract keeps vendors, products with their subtype, and presets with their
author, details, link, preamp gain and bands. Each preset was made by the
author it names (AutoEQ, oratory1990, Rtings/AutoEQ) and is distributed by OPRA.
It was made with `scripts/opra_extract.py` from OPRA's `dist/database_v1.jsonl`
(commit 0b88ecd4e2bef7cf69fd5d50f1d06fb586c10865). It is reformatted and
reduced, and its numbers are unchanged.

`opra-logo.png` is OPRA's logo, from the `images/` folder of the same
repository, used to credit OPRA where its presets are browsed.

OPRA's own code is MIT licensed; the full licence texts are in OPRA's
`LICENSE.md`.
