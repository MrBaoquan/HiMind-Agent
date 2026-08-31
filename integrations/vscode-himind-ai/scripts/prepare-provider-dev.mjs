import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const flavor = process.argv[2] ?? "dev";
if (!new Set(["dev", "preview"]).has(flavor)) {
  throw new Error(`Unsupported provider staging flavor: ${flavor}`);
}
const stageRoot = resolve(extensionRoot, "dist", flavor === "preview" ? "provider-preview" : "provider-dev");
const manifestPath = resolve(extensionRoot, "package.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));

manifest.displayName = `${manifest.displayName} Provider ${flavor === "preview" ? "Preview" : "Dev"}`;
manifest.enabledApiProposals = [...new Set([...(manifest.enabledApiProposals ?? []), "chatProvider"])];
manifest.activationEvents = [
  ...new Set([
    ...(manifest.activationEvents ?? []),
    "onLanguageModelChatProvider:himind",
    "onCommand:himindAi.manageModels",
  ]),
];
manifest.contributes = {
  ...manifest.contributes,
  commands: [
    ...(manifest.contributes?.commands ?? []),
    {
      command: "himindAi.manageModels",
      category: "HiMind AI",
      title: "管理 Copilot Chat 模型",
    },
  ],
  languageModelChatProviders: [
    {
      vendor: "himind",
      displayName: "HiMind",
      managementCommand: "himindAi.connect",
    },
  ],
};

await rm(stageRoot, { recursive: true, force: true });
await mkdir(stageRoot, { recursive: true });
await Promise.all([
  cp(resolve(extensionRoot, "out"), resolve(stageRoot, "out"), { recursive: true }),
  cp(resolve(extensionRoot, "README.md"), resolve(stageRoot, "README.md")),
  cp(resolve(extensionRoot, "LICENSE"), resolve(stageRoot, "LICENSE")),
]);
await writeFile(resolve(stageRoot, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");

console.log(stageRoot);
