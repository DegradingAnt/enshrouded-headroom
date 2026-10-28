"""Independent Draft 2020-12 structural contract check; no production Python dependency."""
import copy
import json
import pathlib
import sys

from jsonschema import Draft202012Validator

crate = pathlib.Path(__file__).resolve().parent.parent
schema = json.loads((crate / "schema/blueprint-v1.schema.json").read_text(encoding="utf-8"))
Draft202012Validator.check_schema(schema)
validator = Draft202012Validator(schema)

paths = list((crate / "examples").glob("*.blueprint.json"))
paths.extend(pathlib.Path(argument) for argument in sys.argv[1:])
for path in paths:
    document = json.loads(path.read_text(encoding="utf-8"))
    validator.validate(document)
    print(f"schema valid: {path.name}")

reference = json.loads((crate / "examples/watchtower.blueprint.json").read_text(encoding="utf-8"))
for field, value in [
    ("schema_version", "2.0"),
    ("source", {}),
    ("unrecognized", True),
]:
    malformed = copy.deepcopy(reference)
    malformed[field] = value
    assert not validator.is_valid(malformed), field
malformed = copy.deepcopy(reference)
malformed["objects"][0]["primitives"][0]["geometry"]["size"][0] = 0
assert not validator.is_valid(malformed), "zero extent"
malformed = copy.deepcopy(reference)
malformed["frame"]["units"] = "feet"
assert not validator.is_valid(malformed), "undeclared unit conversion"
print("schema rejected malformed version, missing source, unknown field, zero extent and units")
