/* tslint:disable */
/* eslint-disable */
export class Walloc {
  private constructor();
  free(): void;
  static new(): Walloc;
  set_base_url(url: string): void;
  load_asset(path: string, asset_type: number): Promise<any>;
  test_fetch_json(): Promise<any>;
  evict_asset(path: string): void;
  get_asset(path: string): Uint8Array;
  get_memory_view(offset: number, length: number): Uint8Array;
  allocate_tiered(size: number, tier_number: number): number;
  fast_compact_tier(tier_number: number, preserve_bytes: number): boolean;
  reset_tier(tier_number: number): boolean;
  copy_from_js(offset: number, data: Uint8Array): void;
  copy_to_js(offset: number, length: number): Uint8Array;
  memory_stats(): object;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly __wbg_walloc_free: (a: number, b: number) => void;
  readonly walloc_new: () => number;
  readonly walloc_set_base_url: (a: number, b: number, c: number, d: number) => void;
  readonly walloc_load_asset: (a: number, b: number, c: number, d: number) => number;
  readonly walloc_test_fetch_json: (a: number) => number;
  readonly walloc_evict_asset: (a: number, b: number, c: number, d: number) => void;
  readonly walloc_get_asset: (a: number, b: number, c: number, d: number) => void;
  readonly walloc_get_memory_view: (a: number, b: number, c: number, d: number) => void;
  readonly walloc_allocate_tiered: (a: number, b: number, c: number) => number;
  readonly walloc_fast_compact_tier: (a: number, b: number, c: number) => number;
  readonly walloc_reset_tier: (a: number, b: number) => number;
  readonly walloc_copy_from_js: (a: number, b: number, c: number, d: number) => void;
  readonly walloc_copy_to_js: (a: number, b: number, c: number, d: number) => void;
  readonly walloc_memory_stats: (a: number) => number;
  readonly __wbindgen_export_0: (a: number) => void;
  readonly __wbindgen_export_1: (a: number, b: number) => number;
  readonly __wbindgen_export_2: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_export_3: WebAssembly.Table;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
  readonly __wbindgen_export_4: (a: number, b: number) => void;
  readonly __wbindgen_export_5: (a: number, b: number, c: number) => void;
  readonly __wbindgen_export_6: (a: number, b: number, c: number, d: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
*
* @returns {InitOutput}
*/
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
