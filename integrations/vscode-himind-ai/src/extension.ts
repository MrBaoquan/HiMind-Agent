import * as vscode from "vscode";
import { promises as fs } from "node:fs";
import { EnrollmentPayload, normalizeBaseUrl, parseEnrollmentUri, parseModelCatalog, parseSseEvent } from "./protocol";
import { discoverEnrollmentHandoffs } from "./enrollmentHandoff";
import { writeImportStatus } from "./importStatus";

const CREDENTIAL_KEY = "himind.ai.credential";
const SELECTED_MODEL_KEY = "himind.ai.selectedModel";
const SMOKE_TEST_ENV = "HIMIND_VSCODE_SMOKE_TEST";
const PROVIDER_PROPOSAL = "chatProvider";
const MANAGE_MODELS_COMMAND = "workbench.action.chat.manage";
const MANAGE_HIMIND_MODELS_COMMAND = "himindAi.manageModels";
const MODEL_REFRESH_TTL_MS = 30_000;
const MODEL_REFRESH_INTERVAL_MS = 60_000;
const COMPLETED_ENROLLMENT_TTL_MS = 2 * 60_000;

type StoredCredential = EnrollmentPayload & { connected_at: string };
type OpenAIMessage = Record<string, unknown>;

// Agent intentionally exposes both a vscode:// URI and a profile-scoped handoff
// file. They can arrive at the same time in separate extension callbacks, so a
// code must share one exchange promise and a short-lived completion marker.
const enrollmentInFlight = new Map<string, Promise<void>>();
const completedEnrollments = new Map<string, number>();

class HiMindProvider implements vscode.LanguageModelChatProvider, vscode.Disposable {
  private readonly modelInformationChanged = new vscode.EventEmitter<void>();
  private modelRefresh: Promise<StoredCredential | undefined> | undefined;
  private lastModelRefreshAt = 0;
  readonly onDidChangeLanguageModelChatInformation = this.modelInformationChanged.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly output: vscode.LogOutputChannel,
  ) {}

  async provideLanguageModelChatInformation(
    _options?: vscode.PrepareLanguageModelChatModelOptions,
    _token?: vscode.CancellationToken
  ): Promise<vscode.LanguageModelChatInformation[]> {
    let credential = await this.readCredential();
    if (!credential) return [];
    try {
      credential = await this.refreshCredentialModels() ?? credential;
    } catch (error) {
      this.output.warn(`Unable to refresh HiMind model catalog; keeping the cached catalog: ${errorMessage(error)}`);
    }
    return credential.models.map((id) => ({
      id,
      name: `HiMind · ${readableModelName(id)}`,
      detail: "HiMind organization gateway",
      tooltip: "Managed by HiMind AI Services",
      family: "himind",
      version: "1.0.0",
      maxInputTokens: 124_000,
      maxOutputTokens: 4_000,
      isUserSelectable: true,
      isBYOK: true,
      capabilities: { toolCalling: true, imageInput: true, agentMode: true },
    }));
  }

  async provideTokenCount(_model: vscode.LanguageModelChatInformation, value: string | vscode.LanguageModelChatRequestMessage): Promise<number> {
    const text = typeof value === "string" ? value : JSON.stringify(toOpenAIMessages([value]));
    return Math.max(1, Math.ceil(text.length / 4));
  }

  async provideLanguageModelChatResponse(
    model: vscode.LanguageModelChatInformation,
    messages: readonly vscode.LanguageModelChatRequestMessage[],
    options: vscode.ProvideLanguageModelChatResponseOptions,
    progress: vscode.Progress<vscode.LanguageModelResponsePart>,
    token: vscode.CancellationToken
  ): Promise<void> {
    let credential = await this.readCredential();
    if (!credential) throw new Error("HiMind is not connected. Open AI Services / My Connections in Dashboard.");
    try {
      credential = await this.refreshCredentialModels() ?? credential;
    } catch (error) {
      this.output.warn(`Unable to refresh HiMind model catalog before request: ${errorMessage(error)}`);
    }
    const body: Record<string, unknown> = {
      model: credential.models.includes(model.id) ? model.id : selectedModel(this.context, credential),
      messages: toOpenAIMessages(messages),
      stream: true,
      stream_options: { include_usage: true },
    };
    const tools = toOpenAITools(options);
    if (tools.length) body.tools = tools;

    const response = await fetch(`${normalizeBaseUrl(credential.base_url)}/chat/completions`, {
      method: "POST",
      headers: { Authorization: `Bearer ${credential.api_key}`, "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: cancellationSignal(token),
    });
    if (!response.ok) throw new Error(`HiMind request failed (${response.status}): ${await response.text()}`);
    if (!response.body) throw new Error("HiMind returned an empty stream");

    const calls = new Map<number, { id: string; name: string; args: string }>();
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (true) {
      const { done, value } = await reader.read();
      buffer += decoder.decode(value ?? new Uint8Array(), { stream: !done });
      const blocks = buffer.split(/\r?\n\r?\n/);
      buffer = blocks.pop() ?? "";
      for (const block of blocks) {
        for (const delta of parseSseEvent(block)) {
          if (delta.type === "text") {
            progress.report(new vscode.LanguageModelTextPart(delta.text));
          } else {
            const current = calls.get(delta.index) ?? { id: "", name: "", args: "" };
            if (delta.id) current.id = delta.id;
            if (delta.name) current.name += delta.name;
            if (delta.arguments) current.args += delta.arguments;
            calls.set(delta.index, current);
          }
        }
      }
      if (done) break;
    }
    for (const call of calls.values()) {
      if (!call.name) continue;
      let input: object = {};
      try { input = JSON.parse(call.args || "{}") as object; } catch { input = {}; }
      progress.report(new vscode.LanguageModelToolCallPart(call.id || `call_${Date.now()}`, call.name, input));
    }
  }

  async readCredential(): Promise<StoredCredential | undefined> {
    const raw = await this.context.secrets.get(CREDENTIAL_KEY);
    if (!raw) return undefined;
    try { return JSON.parse(raw) as StoredCredential; } catch { return undefined; }
  }

  async refreshCredentialModels(force = false): Promise<StoredCredential | undefined> {
    const credential = await this.readCredential();
    if (!credential) return undefined;
    if (!force && Date.now() - this.lastModelRefreshAt < MODEL_REFRESH_TTL_MS) return credential;
    if (this.modelRefresh) return this.modelRefresh;

    this.modelRefresh = this.fetchAndStoreModelCatalog(credential);
    try {
      return await this.modelRefresh;
    } finally {
      this.lastModelRefreshAt = Date.now();
      this.modelRefresh = undefined;
    }
  }

  private async fetchAndStoreModelCatalog(credential: StoredCredential): Promise<StoredCredential> {
    const response = await fetch(`${normalizeBaseUrl(credential.base_url)}/models`, {
      headers: { Authorization: `Bearer ${credential.api_key}` },
    });
    if (!response.ok) throw new Error(`model catalog HTTP ${response.status}: ${await response.text()}`);
    const models = parseModelCatalog(await response.json());
    const current = await this.readCredential();
    if (!current || current.api_key !== credential.api_key || current.base_url !== credential.base_url) {
      return current ?? credential;
    }

    const fallbackModel = current.model && models.includes(current.model) ? current.model : models[0];
    const updated: StoredCredential = { ...current, model: fallbackModel, models };
    const changed = !sameModels(current.models, models);
    if (changed || current.model !== fallbackModel) {
      await this.context.secrets.store(CREDENTIAL_KEY, JSON.stringify(updated));
      const selected = this.context.globalState.get<string>(SELECTED_MODEL_KEY);
      if (!selected || !models.includes(selected)) {
        await this.context.globalState.update(SELECTED_MODEL_KEY, fallbackModel);
      }
      this.modelInformationChanged.fire();
      this.output.info(`HiMind model catalog refreshed: ${models.length} models available.`);
    }
    await writeImportStatus(true, updated);
    return updated;
  }

  refreshModels(): void {
    this.lastModelRefreshAt = 0;
    this.modelInformationChanged.fire();
  }

  dispose(): void {
    this.modelInformationChanged.dispose();
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const output = vscode.window.createOutputChannel("HiMind AI", { log: true });
  const provider = new HiMindProvider(context, output);
  void provider.refreshCredentialModels(true).catch((error) => {
    output.warn(`Unable to refresh HiMind model catalog during activation: ${errorMessage(error)}`);
  });
  const participant = vscode.chat.createChatParticipant("himind.chat", createParticipantHandler(context, provider));
  participant.iconPath = new vscode.ThemeIcon("sparkle");
  output.info("HiMind AI extension activated with the stable Chat Participant API.");
  registerProposedProvider(context, output, provider);
  const handoffTimer = setInterval(() => void consumeEnrollmentHandoff(context, output, provider), 1_500);
  const modelRefreshTimer = setInterval(() => void provider.refreshCredentialModels(true).catch((error) => {
    output.warn(`Unable to refresh HiMind model catalog in the background: ${errorMessage(error)}`);
  }), MODEL_REFRESH_INTERVAL_MS);
  void consumeEnrollmentHandoff(context, output, provider);
  context.subscriptions.push(
    output,
    provider,
    { dispose: () => clearInterval(handoffTimer) },
    { dispose: () => clearInterval(modelRefreshTimer) },
    participant,
    vscode.window.registerUriHandler({ handleUri: async (uri) => {
      if (uri.path === "/disconnect" || uri.path === "disconnect") {
        await disconnect(context, provider, output, false);
      } else {
        await enroll(context, output, provider, uri);
      }
    } }),
    vscode.commands.registerCommand("himindAi.connect", () => {
      void vscode.window.showInformationMessage("请打开 HiMind 工作台，在 AI 服务 / 我的接入中选择 VS Code。");
    }),
    vscode.commands.registerCommand("himindAi.selectModel", () => selectModel(context, provider)),
    vscode.commands.registerCommand("himindAi.testConnection", () => testConnection(provider)),
    vscode.commands.registerCommand(MANAGE_HIMIND_MODELS_COMMAND, () => vscode.commands.executeCommand(MANAGE_MODELS_COMMAND)),
    vscode.commands.registerCommand("himindAi.disconnect", async () => {
      const credential = await provider.readCredential();
      await context.secrets.delete(CREDENTIAL_KEY);
      await writeImportStatus(false, credential);
      await context.globalState.update(SELECTED_MODEL_KEY, undefined);
      provider.refreshModels();
      await vscode.window.showInformationMessage("已断开 HiMind AI 连接。");
    })
  );
}

function registerProposedProvider(
  context: vscode.ExtensionContext,
  output: vscode.LogOutputChannel,
  provider: HiMindProvider
): void {
  const proposals = context.extension.packageJSON.enabledApiProposals;
  const providerEnabled = Array.isArray(proposals) && proposals.includes(PROVIDER_PROPOSAL);
  if (!providerEnabled) return;

  try {
    context.subscriptions.push(vscode.lm.registerLanguageModelChatProvider("himind", provider));
    const providerChannel = context.extensionMode === vscode.ExtensionMode.Development
      ? "development"
      : context.extensionMode === vscode.ExtensionMode.Test
        ? "test"
        : "stable";
    output.info(`HiMind ${providerChannel} Language Model Chat Provider registered for the VS Code model picker.`);
    void provider.provideLanguageModelChatInformation().then(
      async (models) => {
        output.info(`HiMind ${providerChannel} model provider exposes ${models.length} enrolled models.`);
        const registeredModels = await vscode.lm.selectChatModels({ vendor: "himind" });
        output.info(`VS Code language model registry exposes ${registeredModels.length} HiMind models.`);
        if (context.extensionMode === vscode.ExtensionMode.Development) {
          await vscode.commands.executeCommand(MANAGE_MODELS_COMMAND);
          output.info("Opened the VS Code Manage Language Models editor for HiMind development verification.");
        }
      },
      (error) => output.error(`Unable to inspect enrolled HiMind models: ${errorMessage(error)}`)
    );
  } catch (error) {
    output.error(`Unable to register the HiMind ${context.extensionMode === vscode.ExtensionMode.Development ? "development" : "stable"} model provider: ${errorMessage(error)}`);
  }
}

async function consumeEnrollmentHandoff(
  context: vscode.ExtensionContext,
  output: vscode.LogOutputChannel,
  provider: HiMindProvider
): Promise<void> {
  const localAppData = process.env.LOCALAPPDATA;
  if (!localAppData) return;
  let handoffs: string[];
  try {
    handoffs = await discoverEnrollmentHandoffs(localAppData);
  } catch (error) {
    output.warn(`Unable to discover enrollment handoff: ${errorMessage(error)}`);
    return;
  }
  for (const handoffPath of handoffs) {
    const claimedPath = `${handoffPath}.claimed-${process.pid}`;
    try {
      await fs.rename(handoffPath, claimedPath);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
        output.warn(`Unable to claim enrollment handoff: ${errorMessage(error)}`);
      }
      continue;
    }

    try {
      const handoff = JSON.parse(await fs.readFile(claimedPath, "utf8")) as { port?: number; code?: string; expires_at?: number };
      if (!handoff.port || !handoff.code || !handoff.expires_at || handoff.expires_at * 1_000 <= Date.now()) {
        throw new Error("HiMind Agent enrollment handoff is invalid or expired");
      }
      const uri = vscode.Uri.parse(`vscode://himind.himind-ai/enroll/${handoff.port}/${handoff.code}`);
      await enroll(context, output, provider, uri);
    } catch (error) {
      output.error(`Enrollment handoff failed: ${errorMessage(error)}`);
    } finally {
      await fs.unlink(claimedPath).catch(() => undefined);
    }
    // A single VS Code instance should consume only the newest pending request.
    // Older profile handoffs can belong to a different Agent instance and must
    // not overwrite the credential just enrolled by the user.
    return;
  }
}


function createParticipantHandler(context: vscode.ExtensionContext, provider: HiMindProvider): vscode.ChatRequestHandler {
  return async (request, chatContext, response, token) => {
    if (request.command === "model") {
      const model = await selectModel(context, provider);
      if (model) response.markdown(`当前使用 **HiMind · ${readableModelName(model)}**（\`${model}\`）。`);
      return;
    }

    let credential = await provider.readCredential();
    if (!credential) {
      response.markdown("尚未连接 HiMind AI。请先在 HiMind 工作台的 **AI 服务 / 我的接入** 中导入 VS Code。");
      response.button({ command: "himindAi.connect", title: "连接 HiMind AI" });
      return { errorDetails: { message: "HiMind AI 尚未连接" } };
    }

    try {
      credential = await provider.refreshCredentialModels() ?? credential;
    } catch {
      // Continue with the last known catalog; the request itself will report a gateway failure.
    }

    const model = selectedModel(context, credential);
    const body = {
      model,
      messages: toParticipantMessages(chatContext, request.prompt),
      stream: true,
      stream_options: { include_usage: true },
    };

    try {
      await streamChatCompletion(credential, body, token, (text) => response.markdown(text));
      return { metadata: { model } };
    } catch (error) {
      const message = errorMessage(error);
      response.markdown(`HiMind 请求失败：${message}`);
      return { errorDetails: { message } };
    }
  };
}

async function enroll(
  context: vscode.ExtensionContext,
  output: vscode.LogOutputChannel,
  provider: HiMindProvider,
  uri: vscode.Uri
): Promise<void> {
  let code = "";
  try {
    const parsed = parseEnrollmentUri(new URL(uri.toString(true)));
    code = parsed.code;
    const completedAt = completedEnrollments.get(code);
    if (completedAt && Date.now() - completedAt < COMPLETED_ENROLLMENT_TTL_MS) return;

    const existing = enrollmentInFlight.get(code);
    if (existing) {
      // The URI and handoff paths are intentionally redundant. Wait for the
      // first exchange and let it own user-facing success/error notifications.
      await existing.catch(() => undefined);
      return;
    }

    const operation = enrollOnce(context, output, provider, parsed.port, code);
    enrollmentInFlight.set(code, operation);
    try {
      await operation;
      completedEnrollments.set(code, Date.now());
      for (const [completedCode, completedAtValue] of completedEnrollments) {
        if (Date.now() - completedAtValue >= COMPLETED_ENROLLMENT_TTL_MS) {
          completedEnrollments.delete(completedCode);
        }
      }
    } finally {
      enrollmentInFlight.delete(code);
    }
  } catch (error) {
    output.error(`Enrollment failed: ${errorMessage(error)}`);
    void vscode.window.showErrorMessage(`HiMind AI 授权失败：${errorMessage(error)}`);
  }
}

async function enrollOnce(
  context: vscode.ExtensionContext,
  output: vscode.LogOutputChannel,
  provider: HiMindProvider,
  port: number,
  code: string,
): Promise<void> {
  const response = await fetch(`http://127.0.0.1:${port}/vscode/enrollment/exchange`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code }),
  });
  const payload = await response.json() as EnrollmentPayload & { error?: string };
  if (!response.ok) throw new Error(payload.error || `Agent returned HTTP ${response.status}`);
  if (!payload.api_key || !payload.models?.length) throw new Error("Agent returned incomplete HiMind credentials");
  payload.base_url = normalizeBaseUrl(payload.base_url);
  await context.secrets.store(CREDENTIAL_KEY, JSON.stringify({ ...payload, connected_at: new Date().toISOString() }));
  await writeImportStatus(true, payload);
  const currentModel = context.globalState.get<string>(SELECTED_MODEL_KEY);
  if (!currentModel || !payload.models.includes(currentModel)) {
    await context.globalState.update(SELECTED_MODEL_KEY, payload.model || payload.models[0]);
  }
  provider.refreshModels();
  output.info(`Enrollment completed with ${payload.models.length} models.`);
  if (process.env[SMOKE_TEST_ENV] === "1") {
    try {
      await runSmokeTest(payload, output);
    } catch (error) {
      output.error(`Smoke test failed: ${errorMessage(error)}`);
      void vscode.window.showWarningMessage(`HiMind AI 已连接，但模型烟测失败：${errorMessage(error)}`);
    }
  }
  if (providerProposalEnabled(context)) {
    const action = await vscode.window.showInformationMessage(
      `HiMind AI 已连接，可使用 ${payload.models.length} 个模型。可在 Copilot Chat 模型选择器的 Other Models 中选择 HiMind。`,
      "管理 HiMind 模型"
    );
    if (action) await vscode.commands.executeCommand(MANAGE_MODELS_COMMAND);
  } else {
    void vscode.window.showInformationMessage(`HiMind AI 已连接，可使用 ${payload.models.length} 个模型。请在聊天中输入 @himind 开始使用。`);
  }
}

async function runSmokeTest(credential: EnrollmentPayload, output: vscode.LogOutputChannel): Promise<void> {
  const model = credential.model || credential.models[0];
  output.info(`Starting opt-in smoke test with model ${model}.`);
  const response = await fetch(`${normalizeBaseUrl(credential.base_url)}/chat/completions`, {
    method: "POST",
    headers: { Authorization: `Bearer ${credential.api_key}`, "Content-Type": "application/json" },
    body: JSON.stringify({
      model,
      messages: [{ role: "user", content: "Respond with exactly: HIMIND_OK" }],
      stream: false,
      max_tokens: 128,
      temperature: 0,
    }),
  });
  if (!response.ok) throw new Error(`smoke test HTTP ${response.status}: ${await response.text()}`);
  const result = await response.json() as { choices?: Array<{ message?: { content?: string } }> };
  const content = result.choices?.[0]?.message?.content?.trim();
  if (!content) throw new Error("smoke test returned no assistant content");
  output.info(`Smoke test completed successfully with model ${model}.`);
}

async function disconnect(
  context: vscode.ExtensionContext,
  provider: HiMindProvider,
  output: vscode.LogOutputChannel,
  notify: boolean,
): Promise<void> {
  const credential = await provider.readCredential();
  await context.secrets.delete(CREDENTIAL_KEY);
  await writeImportStatus(false, credential);
  await context.globalState.update(SELECTED_MODEL_KEY, undefined);
  provider.refreshModels();
  output.info("HiMind AI credentials cleared from VS Code SecretStorage.");
  if (notify) await vscode.window.showInformationMessage("HiMind AI connection removed.");
}

async function selectModel(context: vscode.ExtensionContext, provider: HiMindProvider): Promise<string | undefined> {
  let credential = await provider.readCredential();
  if (!credential) {
    await vscode.window.showWarningMessage("HiMind AI 尚未连接。");
    return undefined;
  }
  try {
    credential = await provider.refreshCredentialModels(true) ?? credential;
  } catch (error) {
    await vscode.window.showWarningMessage(`暂时无法刷新 HiMind 模型，已显示上次同步结果：${errorMessage(error)}`);
  }
  const current = selectedModel(context, credential);
  const selected = await vscode.window.showQuickPick(
    credential.models.map((id) => ({ label: `HiMind · ${readableModelName(id)}`, description: id, id })),
    { title: "选择 HiMind 模型", placeHolder: `当前：${current}` }
  );
  if (!selected) return undefined;
  await context.globalState.update(SELECTED_MODEL_KEY, selected.id);
  await vscode.window.showInformationMessage(`HiMind 当前模型已切换为 ${selected.id}。`);
  return selected.id;
}

async function testConnection(provider: HiMindProvider): Promise<void> {
  const credential = await provider.readCredential();
  if (!credential) {
    await vscode.window.showWarningMessage("HiMind AI 尚未连接。");
    return;
  }
  try {
    const refreshed = await provider.refreshCredentialModels(true);
    await vscode.window.showInformationMessage(`HiMind AI 连接正常，已同步 ${refreshed?.models.length ?? 0} 个模型。`);
  } catch (error) {
    await vscode.window.showErrorMessage(`HiMind AI 连接失败：${errorMessage(error)}`);
  }
}

function providerProposalEnabled(context: vscode.ExtensionContext): boolean {
  const proposals = context.extension.packageJSON.enabledApiProposals;
  return Array.isArray(proposals) && proposals.includes(PROVIDER_PROPOSAL);
}

function selectedModel(context: vscode.ExtensionContext, credential: StoredCredential): string {
  const selected = context.globalState.get<string>(SELECTED_MODEL_KEY);
  if (selected && credential.models.includes(selected)) return selected;
  if (credential.model && credential.models.includes(credential.model)) return credential.model;
  return credential.models[0];
}

function sameModels(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((model, index) => model === right[index]);
}

function toParticipantMessages(context: vscode.ChatContext, prompt: string): OpenAIMessage[] {
  const messages: OpenAIMessage[] = [];
  for (const turn of context.history) {
    if ("prompt" in turn) {
      messages.push({ role: "user", content: turn.prompt });
      continue;
    }
    const content = turn.response
      .filter((part): part is vscode.ChatResponseMarkdownPart => part instanceof vscode.ChatResponseMarkdownPart)
      .map((part) => part.value.value)
      .join("");
    if (content) messages.push({ role: "assistant", content });
  }
  messages.push({ role: "user", content: prompt });
  return messages;
}

async function streamChatCompletion(
  credential: StoredCredential,
  body: Record<string, unknown>,
  token: vscode.CancellationToken,
  onText: (text: string) => void
): Promise<void> {
  const response = await fetch(`${normalizeBaseUrl(credential.base_url)}/chat/completions`, {
    method: "POST",
    headers: { Authorization: `Bearer ${credential.api_key}`, "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: cancellationSignal(token),
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${await response.text()}`);
  if (!response.body) throw new Error("HiMind 返回了空响应");

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  while (true) {
    const { done, value } = await reader.read();
    buffer += decoder.decode(value ?? new Uint8Array(), { stream: !done });
    const blocks = buffer.split(/\r?\n\r?\n/);
    buffer = blocks.pop() ?? "";
    for (const block of blocks) {
      for (const delta of parseSseEvent(block)) {
        if (delta.type === "text") onText(delta.text);
      }
    }
    if (done) break;
  }
  if (buffer.trim()) {
    for (const delta of parseSseEvent(buffer)) {
      if (delta.type === "text") onText(delta.text);
    }
  }
}

function readableModelName(id: string): string {
  return id.split(/[\/]/).pop()?.replace(/(^|[-_.])([a-z])/g, (_, separator: string, letter: string) => `${separator}${letter.toUpperCase()}`) ?? id;
}

function toOpenAIMessages(messages: readonly vscode.LanguageModelChatRequestMessage[]): OpenAIMessage[] {
  const output: OpenAIMessage[] = [];
  for (const message of messages) {
    const role = message.role === vscode.LanguageModelChatMessageRole.User ? "user" : "assistant";
    const text: string[] = [];
    const calls: Array<Record<string, unknown>> = [];
    for (const part of message.content ?? []) {
      if (part instanceof vscode.LanguageModelTextPart) text.push(part.value);
      else if (part instanceof vscode.LanguageModelToolCallPart) calls.push({ id: part.callId, type: "function", function: { name: part.name, arguments: JSON.stringify(part.input ?? {}) } });
      else if (part instanceof vscode.LanguageModelToolResultPart) {
        output.push({ role: "tool", tool_call_id: part.callId, content: part.content.map((item) => item instanceof vscode.LanguageModelTextPart ? item.value : "").join("") });
      }
    }
    const converted: OpenAIMessage = { role, content: text.join("") };
    if (calls.length) converted.tool_calls = calls;
    if (converted.content || calls.length) output.push(converted);
  }
  return output;
}

function toOpenAITools(options: vscode.ProvideLanguageModelChatResponseOptions): Array<Record<string, unknown>> {
  return (options.tools ?? []).map((tool) => ({ type: "function", function: { name: tool.name, description: tool.description, parameters: tool.inputSchema } }));
}

function cancellationSignal(token: vscode.CancellationToken): AbortSignal {
  const controller = new AbortController();
  if (token.isCancellationRequested) controller.abort();
  token.onCancellationRequested(() => controller.abort());
  return controller.signal;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function deactivate(): void {}
