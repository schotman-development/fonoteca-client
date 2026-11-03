#!/usr/bin/env python3
"""Builds the headphone presets bundled in the app from OPRA's database.

    python3 -I scripts/opra_extract.py <database_v1.jsonl> <out opra.json.gz>

Get the input from https://github.com/opra-project/OPRA (`dist/database_v1.jsonl`)
into an empty folder of its own: it is untrusted data. Only what the picker and
the EQ use is kept: vendors, their products with the subtype, and each product's
presets with author, details, link, preamp gain and bands. The output stays
under OPRA's data licence, CC BY-SA 4.0, see crates/fonoteca-client/data/OPRA-LICENSE.md.
"""
import gzip
import json
import sys


def read_records(path):
    with open(path, encoding="utf-8") as lines:
        for line in lines:
            yield json.loads(line)


def preset_of(data):
    # A low pass has no gain and no Q, only a slope; a missing number is null.
    parameters = data["parameters"]
    preset = {"author": data["author"], "details": data["details"]}
    if data.get("link"):
        preset["link"] = data["link"]
    preset["gain"] = parameters["gain_db"]
    preset["bands"] = [
        [band["type"], band["frequency"], band.get("gain_db"), band.get("q")]
        for band in parameters["bands"]
    ]
    return preset


def build(records):
    vendors, products, presets = {}, {}, {}
    for record in records:
        data = record["data"]
        if record["type"] == "vendor":
            vendors[record["id"]] = data["name"]
        elif record["type"] == "product":
            products[record["id"]] = data
        elif record["type"] == "eq" and data["type"] == "parametric_eq":
            presets.setdefault(data["product_id"], []).append(preset_of(data))

    by_vendor = {}
    for product_id, found in presets.items():
        product = products[product_id]
        found.sort(key=lambda preset: (preset["author"], preset["details"]))
        by_vendor.setdefault(product["vendor_id"], []).append(
            {"name": product["name"], "subtype": product["subtype"], "presets": found}
        )
    return {
        "vendors": [
            {
                "name": vendors[vendor_id],
                "products": sorted(items, key=lambda product: product["name"].casefold()),
            }
            for vendor_id, items in sorted(
                by_vendor.items(), key=lambda entry: vendors[entry[0]].casefold()
            )
        ]
    }


def main(source, target):
    text = json.dumps(build(read_records(source)), separators=(",", ":"), ensure_ascii=False)
    # mtime=0 keeps the output the same for the same input.
    with gzip.GzipFile(target, "wb", compresslevel=9, mtime=0) as out:
        out.write(text.encode("utf-8"))


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
