// 从工作台 AI 服务目录生成 Agent 内置兜底预设（零依赖）。
//
// 事实源是工作台的用户级 AI 服务目录（GCMP 供应商目录投影）。Agent 连上工作台
// 时直接吃目录，本脚本只负责把同一份映射固化成离线兜底，避免手工维护第二份
// 供应商清单后与工作台悄悄漂移。
//
// 用法：
//   1. 保存目录响应：curl -H "Authorization: Bearer <token>" -H "X-HiMind-Actor-ID: <id>" \
//        http://<ai-control>/v1/users/<id>/personal-connections/catalog -o catalog.json
//   2. npm run generate:ai-service-presets -- catalog.json
//
// 映射规则必须与 Rust 侧 src/app/ai_service_templates.rs 的 from_catalog 一致：
// 无接入地址跳过、协议只认识 openai_compatible/anthropic、模型取 upstream_model
// 并去重、默认模型取第一个 recommended。
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const OUTPUT = resolve(dirname(fileURLToPath(import.meta.url)), '../src/pages/aiServicePresets.ts');

type CatalogModel = {
  display_name?: string;
  model_alias?: string;
  upstream_model?: string;
  recommended?: boolean;
};

type CatalogItem = {
  id?: string;
  name?: string;
  category?: string;
  description?: string;
  protocol?: string;
  base_url?: string;
  model_catalog_version?: string;
  models?: CatalogModel[];
};

type Preset = {
  id: string;
  name: string;
  category: string;
  description: string;
  base_url: string;
  protocol: string;
  default_model: string;
  models: string[];
};

function protocolFor(value: string): string | null {
  switch ((value ?? '').trim()) {
    case 'anthropic':
      return 'anthropic';
    case 'openai_compatible':
    case '':
      return 'openai-responses';
    default:
      return null;
  }
}

function modelId(model: CatalogModel): string {
  const upstream = (model.upstream_model ?? '').trim();
  return upstream || (model.model_alias ?? '').trim();
}

function mapCatalog(items: CatalogItem[]): { presets: Preset[]; catalogVersion: string } {
  const presets: Preset[] = [];
  const seen = new Set<string>();
  let catalogVersion = '';
  for (const item of items) {
    const id = (item.id ?? '').trim();
    const baseUrl = (item.base_url ?? '').trim();
    if (!id || !baseUrl || seen.has(id)) continue;
    const protocol = protocolFor(item.protocol ?? '');
    if (!protocol) continue;
    seen.add(id);
    const models: string[] = [];
    for (const model of item.models ?? []) {
      const modelName = modelId(model);
      if (modelName && !models.includes(modelName)) models.push(modelName);
    }
    const recommended = (item.models ?? []).find((model) => model.recommended && modelId(model));
    presets.push({
      id,
      name: (item.name ?? '').trim(),
      category: (item.category ?? '').trim(),
      description: (item.description ?? '').trim(),
      base_url: baseUrl,
      protocol,
      default_model: recommended ? modelId(recommended) : (models[0] ?? ''),
      models,
    });
    if (!catalogVersion && item.model_catalog_version) catalogVersion = item.model_catalog_version.trim();
  }
  return { presets, catalogVersion };
}

function render(presets: Preset[], catalogVersion: string): string {
  const header = [
    '// 本文件由 scripts/generate-ai-service-presets.mts 生成，请勿手工编辑。',
    '//',
    '// 内容来自工作台 AI 服务目录（GET /api/integrations/ai/personal-connections/catalog，',
    '// GCMP 供应商目录投影），生成时目录版本 ' + (catalogVersion || '未知') + '，共 ' + presets.length + ' 条。',
    '// 它只在独立模式或目录暂不可用时兜底；能连上工作台时以工作台目录为准。',
    '// 重新生成：保存目录响应后执行 npm run generate:ai-service-presets -- catalog.json。',
    "import type { AIServiceProtocol } from '../services/agentApi';",
    '',
    'export type AiServicePreset = {',
    '  id: string;',
    '  name: string;',
    '  category: string;',
    '  description: string;',
    '  base_url: string;',
    '  protocol: AIServiceProtocol;',
    '  default_model: string;',
    '  models: string[];',
    '};',
    '',
    'export const fallbackAiServicePresets: AiServicePreset[] = [',
  ];
  const body = presets.map((preset) => [
    '  {',
    "    id: '" + preset.id + "',",
    "    name: '" + preset.name + "',",
    "    category: '" + preset.category + "',",
    "    description: '" + preset.description + "',",
    "    base_url: '" + preset.base_url + "',",
    "    protocol: '" + preset.protocol + "',",
    "    default_model: '" + preset.default_model + "',",
    '    models: [' + preset.models.map((model) => "'" + model + "'").join(', ') + '],',
    '  },',
  ].join('\n')).join('\n');
  return [...header, body, '];', ''].join('\n');
}

const input = process.argv[2];
if (!input) {
  console.error('用法：npm run generate:ai-service-presets -- <catalog.json>');
  process.exit(1);
}
const raw = JSON.parse(readFileSync(input, 'utf8')) as { items?: CatalogItem[] } | CatalogItem[];
const items = Array.isArray(raw) ? raw : (raw.items ?? []);
const { presets, catalogVersion } = mapCatalog(items);
if (!presets.length) {
  console.error('目录里没有任何可用模板，未写入文件。');
  process.exit(1);
}
writeFileSync(OUTPUT, render(presets, catalogVersion), { encoding: 'utf8' });
console.log('已生成 ' + presets.length + ' 条预设 → ' + OUTPUT);
