declare module "vscode" {
  export interface LanguageModelChatInformation {
    id: string;
    name: string;
    detail?: string;
    tooltip?: string;
    family: string;
    version: string;
    maxInputTokens: number;
    maxOutputTokens: number;
    isUserSelectable: boolean;
    isBYOK?: boolean;
    capabilities?: { toolCalling?: boolean; imageInput?: boolean; agentMode?: boolean };
  }

  export interface LanguageModelChatProvider {
    readonly onDidChangeLanguageModelChatInformation?: Event<void>;
    provideLanguageModelChatInformation(options: { silent: boolean }, token: CancellationToken): ProviderResult<LanguageModelChatInformation[]>;
    provideLanguageModelChatResponse(
      model: LanguageModelChatInformation,
      messages: readonly LanguageModelChatRequestMessage[],
      options: ProvideLanguageModelChatResponseOptions,
      progress: Progress<LanguageModelResponsePart>,
      token: CancellationToken
    ): ProviderResult<void>;
    provideTokenCount?(model: LanguageModelChatInformation, text: string | LanguageModelChatRequestMessage, token: CancellationToken): ProviderResult<number>;
  }

  export namespace lm {
    export function registerLanguageModelChatProvider(vendor: string, provider: LanguageModelChatProvider): Disposable;
  }
}
