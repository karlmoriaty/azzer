/**
 * API 客户端。
 *
 * 两种运行环境：
 * - 开发期：Vite 代理代填 Authorization，前端不带 token
 * - 生产（Tauri）：宿主注入 `window.__AZZER__ = { baseUrl, token }`
 *
 * 客户端自动识别，不让调用方关心。
 */

export interface EpReport {
  name: string;
  compiled_in: boolean;
  priority: number;
}

export interface RuntimeProbe {
  ort_version: string;
  providers: EpReport[];
  recommended: string;
  init_error: string | null;
}

export interface SystemInfo {
  version: string;
  uptime_s: number;
  runtime: RuntimeProbe;
  gpu_accelerated: boolean;
  /** 当前实际可用的入口档位。零模型时只有 direct 为 true。 */
  capabilities: { direct: boolean; separate: boolean; convert: boolean };
  models_declared: number;
}

interface HostBridge {
  baseUrl: string;
  token: string;
}

declare global {
  interface Window {
    __AZZER__?: HostBridge;
  }
}

function baseUrl(): string {
  return window.__AZZER__?.baseUrl ?? "";
}

function authHeader(): Record<string, string> {
  const t = window.__AZZER__?.token;
  return t ? { Authorization: `Bearer ${t}` } : {};
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function getJson<T>(path: string): Promise<T> {
  const resp = await fetch(`${baseUrl()}${path}`, {
    headers: { Accept: "application/json", ...authHeader() },
  });
  if (!resp.ok) {
    let code = `HTTP_${resp.status}`;
    let message = `请求失败（${resp.status}）`;
    try {
      const body = (await resp.json()) as { error?: { code?: string; message?: string } };
      if (body.error?.code) code = body.error.code;
      if (body.error?.message) message = body.error.message;
    } catch {
      /* 响应不是 JSON，保留默认文案 */
    }
    throw new ApiError(resp.status, code, message);
  }
  return (await resp.json()) as T;
}

export const api = {
  health: () => getJson<{ ok: boolean; service: string }>("/health"),
  systemInfo: () => getJson<SystemInfo>("/v1/system/info"),
};

// ---------------------------------------------------------------- 处理任务

/** 37 项里的一项。`value` 是标签联合：有值或 not_applicable（带原因）。 */
export interface EvalItem {
  group: string;
  name: string;
  target: string;
  value: { Value: number } | { NotApplicable: string };
  meets: boolean | null;
}

export interface EvalReport {
  items: EvalItem[];
  measured: number;
  not_applicable: number;
  failing: string[];
  humanize_parts: [string, number][];
}

export interface AEntryReport {
  source: string;
  root: string;
  n01_key: string;
  n02_key: string;
  n09_key: string;
  n05_key: string;
  n15_key: string;
  n16_key: string;
  elapsed_ms: number;
  grade: {
    grade: "S" | "A" | "B" | "C";
    snr_db: number;
    peak_dbfs: number;
    clip_ratio: number;
    duration_s: number;
    effective_bandwidth_hz: number;
    rt60_s: number | null;
    reason: string;
  };
  repair: {
    denoise_applied: boolean;
    de_reverb_applied: boolean;
    hf_rebuild_applied: boolean;
    rt60_s: number | null;
    hf_ratio_before: number;
    hf_ratio_after: number;
    meets_hf_ratio: boolean;
    hf_gain_db: number | null;
  };
  f0: {
    total_frames: number;
    voiced_frames: number;
    voiced_ratio: number;
    route_disagreement_ratio: number;
    meets_route_contract: boolean;
    median_f0_hz: number | null;
    min_f0_hz: number | null;
    max_f0_hz: number | null;
  };
  source_lufs: number | null;
  source_dbtp: number;
  master: {
    input: { integrated_lufs: number | null; lra_lu: number | null };
    input_dbtp: number;
    output: { integrated_lufs: number | null; lra_lu: number | null };
    output_dbtp: number;
    gain_db: number;
    limiter_reduction_db: number;
    passes: number;
    meets_lufs: boolean;
    meets_true_peak: boolean;
  };
  chain: {
    mono_lows_applied: boolean;
    dynamic_eq_applied: boolean;
    multiband_applied: boolean;
    saturation_applied: boolean;
    width_applied: boolean;
    peak_after_each: number[];
  };
  eval: EvalReport;
  models_declared: number;
  path_note: string;
}

export type Job =
  | { state: "running" }
  | { state: "done"; report: AEntryReport; master_file: string }
  | { state: "failed"; message: string };

function errorFromBody(status: number, body: unknown): ApiError {
  const b = body as { error?: { code?: string; message?: string } } | null;
  const code = b?.error?.code ?? `HTTP_${status}`;
  const message = b?.error?.message ?? `请求失败（${status}）`;
  return new ApiError(status, code, message);
}

/** 上传源 WAV 原始字节，返回任务号。请求体就是文件本身，不是 multipart。 */
export async function uploadCover(file: File): Promise<{ job_id: string }> {
  const resp = await fetch(`${baseUrl()}/v1/covers`, {
    method: "POST",
    headers: { "Content-Type": "audio/wav", ...authHeader() },
    body: file,
  });
  if (!resp.ok) {
    let body: unknown = null;
    try {
      body = await resp.json();
    } catch {
      /* 非 JSON 响应，用默认文案 */
    }
    throw errorFromBody(resp.status, body);
  }
  return (await resp.json()) as { job_id: string };
}

export function getCover(jobId: string): Promise<Job> {
  return getJson<Job>(`/v1/covers/${jobId}`);
}

/**
 * 取成品音频。
 *
 * 不能直接把 URL 丢给 `<audio src>`：鉴权要带 Authorization 头，
 * 而 `<audio>` 无法自定义头。所以用 fetch 拿 blob 再转 object URL，
 * 用完记得 revoke，否则整段音频会一直占内存。
 */
export async function fetchMasterBlob(jobId: string): Promise<Blob> {
  const resp = await fetch(`${baseUrl()}/v1/covers/${jobId}/master.wav`, {
    headers: authHeader(),
  });
  if (!resp.ok) {
    let body: unknown = null;
    try {
      body = await resp.json();
    } catch {
      /* 非 JSON 响应 */
    }
    throw errorFromBody(resp.status, body);
  }
  return await resp.blob();
}
