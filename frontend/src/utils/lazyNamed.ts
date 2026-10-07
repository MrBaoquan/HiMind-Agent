import { lazy, type ComponentType, type LazyExoticComponent } from 'react';

/**
 * Convert a named page export into a React.lazy component without repeating
 * the same adapter expression in the application entry point.
 */
export function lazyNamed<N extends string, T extends ComponentType<any>>(
  load: () => Promise<{ [K in N]: T }>,
  name: N,
): LazyExoticComponent<T> {
  return lazy(async () => {
    const module = await load();
    return { default: module[name] };
  });
}
