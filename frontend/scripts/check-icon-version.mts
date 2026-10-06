// 图标体系的守卫自检（零依赖：node --experimental-strip-types）。
// 图标形变要求「同一个图标位换图标时几何还能对上」，因此这里锁三件事：
// 1. 形变用的数据包（lucide）和静态图标组件包（lucide-react）必须同版本，
//    版本错开时同一个图标在两个包里可能不是同一条路径，形变会跳一下。
// 2. 形变能力只能从 lucide 的数据包取（lucide-react 的组件里只有 React 元素，
//    拿不到路径数据），静态图标只能从 lucide-react 取，两者不能混着当对方用。
// 3. morphicons 是唯一一处外部形变实现，只允许在 MorphIcon 适配层里出现，
//    否则「换图标库」会变成改 N 个页面。
import { strict as assert } from 'node:assert';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative, sep } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

function readPackage(path: string) {
  return JSON.parse(readFileSync(path, 'utf8')) as { version: string; dependencies?: Record<string, string> };
}

const lucideVersion = readPackage(join(root, 'node_modules', 'lucide', 'package.json')).version;
const lucideReactVersion = readPackage(join(root, 'node_modules', 'lucide-react', 'package.json')).version;
assert.equal(
  lucideVersion,
  lucideReactVersion,
  `lucide(${lucideVersion}) 与 lucide-react(${lucideReactVersion}) 必须同版本，否则形变前后的路径不是同一套几何`,
);

const declared = readPackage(join(root, 'package.json')).dependencies ?? {};
assert.equal(
  declared.lucide?.replace(/^[\^~]/, ''),
  declared['lucide-react']?.replace(/^[\^~]/, ''),
  'package.json 里 lucide 与 lucide-react 的版本范围必须写成同一个',
);

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(name) ? [path] : [];
  });
}

// 允许出现形变数据导入的文件：形变图标位就在这里，换了图标要看得见。README 式的注释不算依赖，
// 所以判据是「这个文件同时用了 MorphIcon」。
const morphDataFiles: string[] = [];
const morphiconsFiles: string[] = [];
for (const file of sourceFiles(join(root, 'src'))) {
  const source = readFileSync(file, 'utf8');
  const short = relative(root, file).split(sep).join('/');
  if (/\bfrom 'lucide'/.test(source)) {
    assert.ok(
      /<MorphIcon/.test(source),
      `${short} 从 lucide 导入了形变数据，但页面里没有 <MorphIcon>：静态图标请从 lucide-react 取`,
    );
    morphDataFiles.push(short);
  }
  if (/from 'morphicons\/react'/.test(source)) morphiconsFiles.push(short);
}
assert.ok(morphDataFiles.length > 0, '形变图标位不存在了：顶栏状态、任务中心状态都依赖它');
assert.deepEqual(
  morphiconsFiles,
  ['src/components/MorphIcon.tsx'],
  'morphicons 只能出现在 MorphIcon 适配层里，其余位置复用这个封装',
);

const busyIndicator = readFileSync(join(root, 'src', 'components', 'BusyIndicator.tsx'), 'utf8');
assert.ok(/from 'lucide-react'/.test(busyIndicator), 'BusyIndicator 是静态图标，只能从 lucide-react 取');
assert.ok(/reducedMotion="never"/.test(readFileSync(join(root, 'src', 'components', 'MorphIcon.tsx'), 'utf8')), '形变必须始终播放，不能跟随系统的「减少动态效果」开关');

// 动效按设计速度常开：整套 reduce 分支已按产品取舍移除（同一套界面不该在不同机器上
// 呈现两种形态），状态反馈的持续感全部靠这些常开的动效承载。
const styles = readFileSync(join(root, 'styles.css'), 'utf8');
assert.ok(/\.busy-indicator \{[^}]*animation: spin/.test(styles), 'BusyIndicator 必须转，否则「正在处理」没有持续反馈');
assert.ok(
  !/prefers-reduced-motion/.test(styles),
  'styles.css 里不该再有 reduce 分支：动效不再跟随系统开关，出现即说明有人把它加回来了',
);
// 流光条纹曾叠在进度条上：背景位移动画上不了合成线程，实测是本套 UI 里最贵的一段，
// 信息量却近乎为零（宽度本身已经在表达进度）。谁要加回来，先看性能数据。
assert.ok(
  !/progress-stripes/.test(styles),
  '流光条纹已移除，不要再以背景位移的形式给进度条加动效（用宽度或明暗表达）',
);

// 4. 每类能力的默认图标：市场、我的能力、扩展开发三处都要能一眼指认「这是什么」，
//    所以每类必须解析得出一个图标，图标位（形状 + 配色）也必须在 styles.css 里齐备。
const kinds = ['plugin', 'skill', 'workflow', 'expert', 'instruction'] as const;
const kindsSource = readFileSync(join(root, 'src', 'data', 'extensionKinds.ts'), 'utf8');
assert.ok(
  /export type ExtensionKind = 'plugin' \| 'skill' \| 'workflow' \| 'expert' \| 'instruction';/.test(kindsSource),
  '能力类型口径变了：图标位按 plugin / skill / workflow / expert / instruction 定义，加类型要同步改这里',
);
const markSource = readFileSync(join(root, 'src', 'components', 'ExtensionKindMark.tsx'), 'utf8');
assert.ok(!/from 'lucide'/.test(markSource), 'ExtensionKindMark 是静态图标位，只能从 lucide-react 取图标');
for (const kind of kinds) {
  assert.ok(
    new RegExp(`^\\s*${kind}: \\w+,`, 'm').test(markSource),
    `ExtensionKindMark 缺少 ${kind} 的默认图标，市场列表行会变成没有类型的空块`,
  );
  assert.ok(
    new RegExp(`\\.extension-kind-mark\\.${kind} \\{`).test(styles),
    `styles.css 缺少 .extension-kind-mark.${kind} 的配色，图标位会退成灰底`,
  );
}
// 类型只能靠这一套图标指认：字母占位（P/S/W）和纯文字类型徽标都已下线，谁加回来谁负责解释。
for (const file of ['ExtensionsPage.tsx', 'InstalledPage.tsx', 'ExtensionDevelopmentPage.tsx', 'PluginsPage.tsx', 'SkillsWorkspacePage.tsx', 'WorkflowsPage.tsx']) {
  const source = readFileSync(join(root, 'src', 'pages', file), 'utf8');
  assert.ok(!/extension-kind-badge/.test(source), `${file} 又用回了纯文字类型徽标，类型要靠 ExtensionKindMark 的图标表达`);
  assert.ok(!/development-kind-mark/.test(source), `${file} 又用回了 P/S/W 字母占位`);
}

console.log('icon version checks passed');
