export function initialize(): Promise<void>;
export function evaluate(request: string): string;

declare global {
  var DenTizen3Core:
    | {
        memory: { buffer: ArrayBuffer };
        den_alloc(length: number): number;
        den_free(pointer: number, length: number): void;
        den_evaluate(input: number, length: number, output: number): void;
      }
    | undefined;
}
