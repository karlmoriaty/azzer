import type { CSSProperties } from "react";

/**
 * 品牌资产的运行时版本，与 design/brand/ 下的 SVG 一一对应。
 *
 * - 01 A / Waveform 是品牌主标，只出现在顶栏、favicon、桌面图标。
 * - 02 Bare Line / 03 Contour / 04 Source → Target 是按钮图标，不单独当装饰用。
 *
 * 描边统一走 currentColor，颜色交给父级 CSS：这样暗色主题、hover、disabled
 * 都只需要改一处，不必再维护一套彩色 SVG 变体。唯一例外是 04 的两个端点，
 * 它靠青绿/暖石的区分表达「源 → 成品」，保留双色。
 */

export type MarkProps = {
  size?: number;
  className?: string;
  style?: CSSProperties;
};

const svgBase = (size: number) => ({
  width: size,
  height: size,
  viewBox: "0 0 120 120",
  fill: "none" as const,
  display: "block" as const,
  flex: "none" as const,
});

/** 01 · 品牌主标。字母 A 的横杠换成一道波形。 */
export function BrandMark({ size = 24, className, style }: MarkProps) {
  return (
    <svg
      {...svgBase(size)}
      className={className}
      style={style}
      role="img"
      aria-label="Azzer"
    >
      <path
        d="M27 94 L60 24 L93 94"
        stroke="currentColor"
        strokeWidth="7"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M43 70 Q48 64 53 70 T63 70 T73 70"
        stroke="currentColor"
        strokeWidth="5"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** 02 · Bare Line：处理应该是平的，人是唯一不平的地方。 */
export function BareLineIcon({ size = 18, className, style }: MarkProps) {
  return (
    <svg
      {...svgBase(size)}
      className={className}
      style={style}
      aria-hidden="true"
      focusable="false"
    >
      <path
        d="M12 60 L44 60 Q49 53 54 60 T64 60 Q69 67 74 60 L108 60"
        stroke="currentColor"
        strokeWidth="9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/**
 * 03 · Contour：剥到最深处，剩下的还是这个人自己的形状。
 * 原稿的环只占 viewBox 一半宽、四分之一高，20px 下会糊成一团；按钮版
 * 整体放大 1.5 倍居中、描边加粗到 9px、提高外圈对比度，实际尺寸不缩到 16px。
 */
export function ContourIcon({ size = 18, className, style }: MarkProps) {
  return (
    <svg
      {...svgBase(size)}
      className={className}
      style={style}
      aria-hidden="true"
      focusable="false"
    >
      <g
        transform="translate(60 60) scale(1.5) translate(-60 -48.5)"
        stroke="currentColor"
        strokeWidth="9"
        strokeLinecap="round"
      >
        <path d="M31 60 A29 29 0 1 1 89 60" opacity="0.55" />
        <path d="M43 60 A17 17 0 1 1 77 60" opacity="0.8" />
        <path d="M51 60 Q55 54 59 60 T67 60 T73 60" />
      </g>
    </svg>
  );
}

/** 04 · Source → Target：中间连着的必须是波形，拉直就等于把人抹平了。 */
export function SourceTargetIcon({ size = 18, className, style }: MarkProps) {
  return (
    <svg
      {...svgBase(size)}
      className={className}
      style={style}
      aria-hidden="true"
      focusable="false"
    >
      <circle cx="24" cy="60" r="10" fill="var(--accent-cool)" />
      <circle cx="96" cy="60" r="10" fill="var(--accent-warm)" />
      <path
        d="M24 60 Q40 38 60 60 T96 60"
        stroke="var(--accent-warm)"
        strokeWidth="7"
        strokeLinecap="round"
      />
    </svg>
  );
}
