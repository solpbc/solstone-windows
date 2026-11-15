declare module "*.mjs" {
  export function render(locale: string, id: string, args: unknown[], options?: { useIsolating: boolean }): string;
  export function integer(value: unknown): unknown;
  export function cardinal(value: unknown): unknown;
}
