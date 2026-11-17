"""Workflow execution-field schema regressions; run with Python + jsonschema."""

import copy
import json
from pathlib import Path
import unittest

from jsonschema import Draft202012Validator


SCHEMA_PATH = Path(__file__).resolve().parents[1] / "workflow-package.schema.json"


def team_package():
    runtime = {
        "provider": "himind.builtin",
        "prompt": "审查本次改动",
        "permission_mode": "full_access",
        "allow_network": None,
        "tool_policy": "files_only",
        "model": "himind:service:model",
        "model_fallbacks": ["himind:service:fallback"],
    }
    return {
        "schema_version": "workflow_package.v1",
        "id": "com.example.workflow.team",
        "version": "1.0.0",
        "name": "团队交付",
        "min_agent_version": "0.3.90",
        "supported_runtimes": ["himind.builtin"],
        "roles": {"reviewer": {
            "optional": True,
            "model": "himind:service:review",
            "tool_policy": "files_only",
        }},
        "capability_alternatives": {
            "distribution.release.publish": ["distribution.release.publish.github"]
        },
        "steps": [{
            "id": "TEAM", "title": "团队审查", "kind": "delegate",
            "execution_mode": "sync", "role_id": "reviewer",
            "delegate": {
                "strategy": "sequential", "min_succeeded": 2,
                "branches": [
                    {"id": "A", "title": "审查", "role_id": "reviewer", "runtime": runtime},
                    {"id": "B", "title": "测试", "runtime": copy.deepcopy(runtime)},
                ],
            },
        }],
        "artifacts": [],
        "ui": {"mode": "standard"},
    }


class WorkflowPackageSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
        Draft202012Validator.check_schema(schema)
        cls.validator = Draft202012Validator(schema)

    def test_current_role_and_runtime_fields(self):
        self.validator.validate(team_package())

    def test_legacy_role_annotation(self):
        package = team_package()
        del package["roles"]
        del package["steps"][0]["delegate"]["strategy"]
        self.validator.validate(package)

    def test_unknown_execution_fields_fail(self):
        for target in ("role", "delegate", "branch", "runtime"):
            with self.subTest(target=target):
                package = team_package()
                delegate = package["steps"][0]["delegate"]
                objects = {
                    "role": package["roles"]["reviewer"],
                    "delegate": delegate,
                    "branch": delegate["branches"][0],
                    "runtime": delegate["branches"][0]["runtime"],
                }
                objects[target]["unknown_execution_field"] = True
                self.assertFalse(self.validator.is_valid(package))

    def test_delegate_structure_failures(self):
        for change in ("single_branch", "nine_branches", "missing_runtime", "strategy", "threshold", "role_id", "input"):
            with self.subTest(change=change):
                package = team_package()
                delegate = package["steps"][0]["delegate"]
                branch = delegate["branches"][0]
                if change == "single_branch":
                    delegate["branches"] = [branch]
                elif change == "nine_branches":
                    delegate["branches"] = [branch] * 9
                elif change == "missing_runtime":
                    del branch["runtime"]
                elif change == "strategy":
                    delegate["strategy"] = "race"
                elif change == "threshold":
                    delegate["min_succeeded"] = -1
                elif change == "role_id":
                    branch["role_id"] = "bad role"
                else:
                    branch["input"] = []
                self.assertFalse(self.validator.is_valid(package))

    def test_invalid_runtime_fields(self):
        for field, value in (
            ("permission_mode", "root"), ("tool_policy", "all"),
            ("allow_network", "false"), ("model", "bare-model"),
            ("model", "himind:service:bad model"),
            ("model_fallbacks", ["bare-model"]),
            ("model_fallbacks", [""]),
        ):
            with self.subTest(field=field, value=value):
                package = team_package()
                package["steps"][0]["delegate"]["branches"][0]["runtime"][field] = value
                self.assertFalse(self.validator.is_valid(package))

    def test_role_metadata_types(self):
        for field, value in (("optional", "true"), ("model", "bare-model")):
            with self.subTest(field=field):
                package = team_package()
                package["roles"]["reviewer"][field] = value
                self.assertFalse(self.validator.is_valid(package))


if __name__ == "__main__":
    unittest.main()
