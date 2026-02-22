#!/usr/bin/env python3
"""Validate ski resort index JSON against schema."""

import argparse
import json
import sys
from pathlib import Path

import jsonschema


def load_schema() -> dict:
    """Load the resort JSON schema."""
    schema_path = Path(__file__).parent.parent / "schemas" / "resort.json"
    with open(schema_path) as f:
        return json.load(f)


def validate_schema(data: dict, schema: dict) -> list[str]:
    """Validate JSON against schema. Returns list of errors."""
    errors = []
    validator = jsonschema.Draft7Validator(schema)
    for error in validator.iter_errors(data):
        path = ".".join(str(p) for p in error.path) or "root"
        errors.append(f"Schema error at {path}: {error.message}")
    return errors


def validate_linking(data: dict) -> list[str]:
    """Ensure linking between relations and areas is consistent."""
    errors = []
    resort_ids = {r["id"] for r in data["resorts"]}

    for resort in data["resorts"]:
        # Validate site_relation_ids
        site_rel_ids = resort.get("site_relation_ids", [])
        for rel_id in site_rel_ids:
            if rel_id not in resort_ids:
                errors.append(
                    f"Resort {resort['id']} ({resort['name']}) references non-existent site_relation_id {rel_id}"
                )
            else:
                # Verify the relation has this area in contained_area_ids
                rel = next((r for r in data["resorts"] if r["id"] == rel_id), None)
                if rel and resort["id"] not in rel.get("contained_area_ids", []):
                    errors.append(
                        f"Relation {rel_id} ({rel['name']}) does not list {resort['id']} ({resort['name']}) in contained_area_ids"
                    )

        # Validate contained_area_ids (for relations)
        contained_ids = resort.get("contained_area_ids", [])
        for area_id in contained_ids:
            if area_id not in resort_ids:
                errors.append(
                    f"Relation {resort['id']} ({resort['name']}) references non-existent contained_area_id {area_id}"
                )
            else:
                # Verify the area has this relation in site_relation_ids
                area = next((r for r in data["resorts"] if r["id"] == area_id), None)
                if area and resort["id"] not in area.get("site_relation_ids", []):
                    errors.append(
                        f"Area {area_id} ({area['name']}) does not list {resort['id']} ({resort['name']}) in site_relation_ids"
                    )

    return errors


def validate_bboxes(data: dict) -> list[str]:
    """Ensure bboxes are valid (west < east, south < north)."""
    errors = []

    for resort in data["resorts"]:
        bbox = resort.get("bbox", [])
        if len(bbox) != 4:
            errors.append(
                f"Resort {resort['id']} ({resort['name']}) has invalid bbox length: {len(bbox)}"
            )
            continue

        west, south, east, north = bbox
        if west >= east:
            errors.append(
                f"Resort {resort['id']} ({resort['name']}) has west >= east: {west} >= {east}"
            )
        if south >= north:
            errors.append(
                f"Resort {resort['id']} ({resort['name']}) has south >= north: {south} >= {north}"
            )

        if not (-180 <= west <= 180 and -180 <= east <= 180):
            errors.append(
                f"Resort {resort['id']} ({resort['name']}) has invalid longitude"
            )
        if not (-90 <= south <= 90 and -90 <= north <= 90):
            errors.append(
                f"Resort {resort['id']} ({resort['name']}) has invalid latitude"
            )

    return errors


def validate_counts(data: dict) -> list[str]:
    """Validate that counts match actual data."""
    errors = []

    actual_count = len(data.get("resorts", []))
    declared_count = data.get("total_resorts", 0)
    if actual_count != declared_count:
        errors.append(
            f"total_resorts mismatch: declared {declared_count}, actual {actual_count}"
        )

    return errors


def validate(input_path: str) -> bool:
    """Main validation function. Returns True if valid."""
    with open(input_path) as f:
        data = json.load(f)

    schema = load_schema()
    all_errors = []

    print("Validating schema...", file=sys.stderr)
    all_errors.extend(validate_schema(data, schema))

    print("Validating linking...", file=sys.stderr)
    all_errors.extend(validate_linking(data))

    print("Validating bboxes...", file=sys.stderr)
    all_errors.extend(validate_bboxes(data))

    print("Validating counts...", file=sys.stderr)
    all_errors.extend(validate_counts(data))

    if all_errors:
        print(f"\nValidation FAILED with {len(all_errors)} errors:", file=sys.stderr)
        for error in all_errors[:20]:
            print(f"  - {error}", file=sys.stderr)
        if len(all_errors) > 20:
            print(f"  ... and {len(all_errors) - 20} more errors", file=sys.stderr)
        return False

    print(f"\nValidation PASSED for {data['total_resorts']} ski areas", file=sys.stderr)
    return True


def main():
    parser = argparse.ArgumentParser(description="Validate ski resort index JSON")
    parser.add_argument("input", help="Input resorts.json file")
    args = parser.parse_args()

    success = validate(args.input)
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
