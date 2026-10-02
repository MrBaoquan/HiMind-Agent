import { useEffect, useState } from 'react';

/**
 * 时间字段在本机契约里有三种历史形态：unix 秒、unix 毫秒（`unix-ms:` 前缀，来自扩展开发草稿），
 * 以及 ISO 字符串。以前每个页面各写一份解析，遇到第三种形态就把原始值直接画到了界面上。
 * 这里只保留一份解析口径，显示层再决定格式。
 */
const UNIX_MS_PREFIX = 'unix-ms:';

export function parseStamp(value: unknown): Date | null {
  if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
    return new Date(value * 1000);
  }
  const text = typeof value === 'string' ? value.trim() : '';
  if (!text) return null;
  if (text.startsWith(UNIX_MS_PREFIX)) {
    const millis = Number(text.slice(UNIX_MS_PREFIX.length));
    return Number.isFinite(millis) && millis > 0 ? new Date(millis) : null;
  }
  const numeric = Number(text);
  if (Number.isFinite(numeric) && numeric > 0) {
    // 契约里纯数字是 unix 秒；13 位以上才当作毫秒，避免把秒当毫秒放大 1000 倍。
    return new Date(text.length >= 13 ? numeric : numeric * 1000);
  }
  const parsed = Date.parse(text);
  return Number.isFinite(parsed) ? new Date(parsed) : null;
}

/**
 * 统一的时间展示：精确到分钟即可，秒级精度只留给「运行中耗时」这类需要持续变化的地方。
 * 解析不了就原样返回，方便定位是哪一个字段格式不符。
 */
export function formatStamp(value: unknown, options: Intl.DateTimeFormatOptions = {}): string {
  if (value === null || value === undefined || value === '') return '--';
  const date = parseStamp(value);
  if (!date) return typeof value === 'string' ? value.trim() || '--' : '--';
  return date.toLocaleString('zh-CN', { year: 'numeric', month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false, ...options });
}

function pad(part: number) {
  return String(part).padStart(2, '0');
}

/**
 * 列表里的紧凑写法：能说「今天 09:00」就不铺完整日期，跨年才补年份。
 * 定时计划这类窄栏里，完整时间戳会把同一行的其它信息挤掉。
 */
export function formatCompactStamp(value: unknown, nowMs = Date.now()): string {
  const date = parseStamp(value);
  if (!date) return value ? String(value) : '--';
  const clock = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  const startOfDay = (input: Date) => new Date(input.getFullYear(), input.getMonth(), input.getDate()).getTime();
  const now = new Date(nowMs);
  const dayDiff = Math.round((startOfDay(date) - startOfDay(now)) / 86_400_000);
  if (date.getFullYear() !== now.getFullYear()) return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${clock}`;
  if (dayDiff === 0) return `今天 ${clock}`;
  if (dayDiff === 1) return `明天 ${clock}`;
  if (dayDiff === 2) return `后天 ${clock}`;
  if (dayDiff > 2 && dayDiff < 7) return `${['周日', '周一', '周二', '周三', '周四', '周五', '周六'][date.getDay()]} ${clock}`;
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${clock}`;
}

/**
 * 需要「持续动态」的地方（运行中耗时、心跳）共用同一个秒级心跳，
 * 空闲时完全不起定时器，避免每个页面各写一份轮询。
 */
export function useNowTick(active: boolean, intervalMs = 1000): number {
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNowMs(Date.now());
    const timer = window.setInterval(() => setNowMs(Date.now()), intervalMs);
    return () => window.clearInterval(timer);
  }, [active, intervalMs]);
  return nowMs;
}

/**
 * 距下一个时间点还有多久。定时计划这类「等在那里」的状态，
 * 倒计时比绝对时间更能说明「现在处于哪个阶段」。
 */
export function formatCountdown(value: unknown, nowMs = Date.now()): string {
  const date = parseStamp(value);
  if (!date) return '';
  const diff = date.getTime() - nowMs;
  if (diff <= 0) return '即将执行';
  const totalMinutes = Math.floor(diff / 60_000);
  const days = Math.floor(totalMinutes / 1440);
  const hours = Math.floor((totalMinutes % 1440) / 60);
  const minutes = totalMinutes % 60;
  if (days > 0) return `还有 ${days} 天${hours ? ` ${hours} 小时` : ''}`;
  if (hours > 0) return `还有 ${hours} 小时 ${minutes} 分`;
  if (minutes > 0) return `还有 ${minutes} 分钟`;
  return '即将执行';
}

/**
 * 相对时间：运行中心里「多久没有新事件」是判断「还在跑 / 卡住了」的主要依据，
 * 绝对时间戳反而要用户自己减一遍。
 */
export function formatRelativeStamp(value: unknown, nowMs = Date.now()): string {
  const date = parseStamp(value);
  if (!date) return '';
  const diffSeconds = Math.round((nowMs - date.getTime()) / 1000);
  if (diffSeconds < 0) return '刚刚';
  if (diffSeconds < 5) return '刚刚';
  if (diffSeconds < 60) return `${diffSeconds} 秒前`;
  const minutes = Math.floor(diffSeconds / 60);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return `${Math.floor(hours / 24)} 天前`;
}

/**
 * 定时计划的时区来自本机 UTC 偏移（后端给的是 `%z`，形如 +0800）。
 * 直接写「按 +0800 时区运行」既啰嗦又不好认，这里统一成 UTC+08:00。
 */
export function formatTimezone(value: unknown): string {
  const raw = String(value ?? '').trim();
  if (!raw) return '本机时区';
  const offset = /^([+-])(\d{2})(\d{2})$/.exec(raw);
  if (offset) return `UTC${offset[1]}${offset[2]}:${offset[3]}`;
  return raw;
}
