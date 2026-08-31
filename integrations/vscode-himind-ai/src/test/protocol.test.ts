import assert from "node:assert/strict";
import test from "node:test";
import { normalizeBaseUrl, parseEnrollmentUri, parseModelCatalog, parseSseEvent } from "../protocol";

test("validates enrollment links", () => {
  const result = parseEnrollmentUri(new URL(`vscode://himind.himind-ai/enroll/18181/${"A".repeat(48)}`));
  assert.deepEqual(result, { port: 18181, code: "A".repeat(48) });
  assert.deepEqual(
    parseEnrollmentUri(new URL(`vscode://himind.himind-ai/enroll?port=18181&code=${"B".repeat(48)}`)),
    { port: 18181, code: "B".repeat(48) }
  );
  assert.throws(() => parseEnrollmentUri(new URL(`vscode://other/enroll/18181/${"A".repeat(48)}`)));
});

test("normalizes gateway URLs", () => {
  assert.equal(normalizeBaseUrl("https://ai.example.com/v1/?ignored=true#x"), "https://ai.example.com/v1");
});

test("parses and deduplicates OpenAI model catalogs", () => {
  assert.deepEqual(parseModelCatalog({
    data: [
      { id: "model-a", object: "model" },
      { id: " model-b " },
      { id: "model-a" },
      { id: "" },
      {},
    ],
  }), ["model-a", "model-b"]);
  assert.throws(() => parseModelCatalog({ data: [] }), /empty model catalog/);
  assert.throws(() => parseModelCatalog({ models: [] }), /invalid model catalog/);
});

test("parses text and tool call SSE deltas", () => {
  const deltas = parseSseEvent('data: {"choices":[{"delta":{"content":"ok","tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\\""}}]}}]}');
  assert.equal(deltas[0]?.type, "text");
  assert.equal(deltas[1]?.type, "tool");
});
