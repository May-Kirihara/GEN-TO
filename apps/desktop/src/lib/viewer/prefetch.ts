/**
 * 先読み (prefetch) プランナ (WP-6 / §13「次・前画像を先読みする」)。
 *
 * 現在indexの前後N件を、進行方向側を優先する順序で返す。
 * 呼び出し側でready地図と突き合わせて、生成済みはURLウォーム、
 * 未生成は生成要求 (prefetch優先度) へ振り分ける。
 */

export type PrefetchPlan = Array<{ index: number; relation: "next" | "prev" }>;

export function planPrefetch(
  current: number,
  total: number,
  count: number,
  direction: 1 | -1 | 0,
): PrefetchPlan {
  if (total <= 0 || count <= 0) {
    return [];
  }
  const pushSide = (step: 1 | -1, plan: PrefetchPlan): void => {
    for (let i = 1; i <= count; i++) {
      const index = current + step * i;
      if (index < 0 || index >= total) {
        break;
      }
      plan.push({ index, relation: step === 1 ? "next" : "prev" });
    }
  };
  const plan: PrefetchPlan = [];
  const forward: 1 | -1 = direction >= 0 ? 1 : -1;
  pushSide(forward, plan);
  pushSide(forward === 1 ? -1 : 1, plan);
  return plan;
}
