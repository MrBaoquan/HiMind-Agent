// AI 服务预设模板。仅收录 OpenAI Chat/Responses 兼容的供应商（HiMind 网关协议约束），
// 参考 gcmp provider_templates 的 openai_compatible 类目；选中后自动填充表单，仍可手改。
export type AiServicePreset = {
  id: string;
  name: string;
  category: string;
  description: string;
  base_url: string;
  protocol: 'openai-chat' | 'openai-responses';
  default_model: string;
  models: string[];
};

export const aiServicePresets: AiServicePreset[] = [
  {
    id: 'openai',
    name: 'OpenAI API',
    category: '国际厂商',
    description: 'OpenAI 官方按量 API',
    base_url: 'https://api.openai.com/v1',
    protocol: 'openai-responses',
    default_model: 'gpt-4.1',
    models: ['gpt-4.1', 'gpt-4.1-mini', 'gpt-4o', 'o3'],
  },
  {
    id: 'deepseek',
    name: 'DeepSeek API',
    category: '国内厂商',
    description: 'DeepSeek 官方按量 API',
    base_url: 'https://api.deepseek.com/v1',
    protocol: 'openai-responses',
    default_model: 'deepseek-v4-flash',
    models: ['deepseek-v4-flash', 'deepseek-v4-flash-vision-exp', 'deepseek-v4-pro'],
  },
  {
    id: 'dashscope',
    name: '阿里云百炼',
    category: '国内厂商',
    description: '百炼 OpenAI 兼容按量 API',
    base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    protocol: 'openai-responses',
    default_model: 'kimi/kimi-k3',
    models: ['kimi/kimi-k3', 'MiniMax/MiniMax-M3'],
  },
  {
    id: 'xiaomi',
    name: '小米 MiMo',
    category: '国内厂商',
    description: '小米 MiMo OpenAI 兼容 API',
    base_url: 'https://api.xiaomimimo.com/v1',
    protocol: 'openai-responses',
    default_model: 'mimo-v2.5-pro',
    models: ['mimo-v2.5-pro-ultraspeed', 'mimo-v2.5-pro', 'mimo-v2.5'],
  },
  {
    id: 'openrouter',
    name: 'OpenRouter',
    category: '聚合平台',
    description: 'OpenRouter 多厂商聚合 API',
    base_url: 'https://openrouter.ai/api/v1',
    protocol: 'openai-responses',
    default_model: 'openai/gpt-4.1',
    models: ['openai/gpt-4.1', 'anthropic/claude-sonnet-4', 'google/gemini-2.5-pro', 'deepseek/deepseek-chat'],
  },
  {
    id: 'siliconflow',
    name: '硅基流动',
    category: '聚合平台',
    description: 'SiliconFlow OpenAI 兼容 API',
    base_url: 'https://api.siliconflow.cn/v1',
    protocol: 'openai-responses',
    default_model: 'deepseek-ai/DeepSeek-V3',
    models: ['deepseek-ai/DeepSeek-V3', 'Qwen/Qwen2.5-Coder-32B-Instruct', 'deepseek-ai/DeepSeek-R1', 'Qwen/Qwen3-32B'],
  },
  {
    id: 'himind',
    name: 'HiMind 网关',
    category: '本机',
    description: 'HiMind 网关 OpenAI 兼容接入点',
    base_url: 'http://127.0.0.1:18182/gateway/v1',
    protocol: 'openai-responses',
    default_model: 'kimi-k3',
    models: ['kimi-k3', 'kimi-for-coding'],
  },
];
