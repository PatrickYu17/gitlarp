declare module "bun:test" {
  export function describe(label: string, fn: () => void): void;
  export function test(label: string, fn: () => void | Promise<void>, timeout?: number): void;
  export const expect: any;
}
