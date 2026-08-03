// Cost advice: look at the dollars providers report and say the one thing worth acting
// on — spend prepaid balance before paying for more elsewhere. Lives outside the
// component so `node src/lib/advisor.test.js` can prove it without a DOM.
//
// Same money rule as `costSummary` in tiles.js: `spend_usd` is a trailing-window SPEND
// (OpenAI's last 30 days), and `credits` whose `credits_unit` is "USD" is a remaining
// BALANCE (DeepSeek's USD wallet, OpenRouter, xAI, OpenAI credits). A CNY wallet or a
// token count carries a different unit and deliberately never reaches dollar advice.
//
// Each advice item is shaped for display:
// {
//   kind: "balance" | "spend",
//   // The sentence, WITHOUT the dollar figure: the component leads with `usd(amount)`,
//   // so locale formatting happens in exactly one place (format.js) and these strings
//   // stay plain text that tests can compare anywhere.
//   text: string,
//   amount: number,      // USD, always finite, always > 0
//   providers: string[], // display names of the providers this item is about
// }

// Snapshots carry only `provider_id`; the display names live in the backend's provider
// registry, which a pure function over snapshots cannot see. Map the ids that report
// dollars today; an unknown id falls back to itself, which is still legible.
const NAMES = {
  deepseek: "DeepSeek",
  openai: "OpenAI",
  openrouter: "OpenRouter",
  xai: "xAI",
};
const nameOf = (id) => NAMES[id] ?? id;

// "A", "A and B", "A, B and C".
function joinNames(names) {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

// The headline advice first: money already paid for beats money about to leave.
//
// Returns [] — never null, never a hedge — whenever nothing on screen reports dollars
// or nothing is actionable. A card that appears with nothing to say teaches the user to
// ignore the card, so empty is the honest default and the component renders nothing.
export function costAdvice(snapshots) {
  const list = Array.isArray(snapshots) ? snapshots : [];

  let balance = 0;
  const balanceNames = [];
  let spend = 0;
  const spendNames = [];

  for (const s of list) {
    if (!s) continue;

    // Only prepaid DOLLARS count: the unit must say USD and the number must be real and
    // positive. Zero or negative is nothing to spend; a missing unit, CNY, or a token
    // count is not dollars.
    if (s.credits_unit === "USD" && Number.isFinite(s.credits) && s.credits > 0) {
      balance += s.credits;
      const name = nameOf(s.provider_id);
      if (!balanceNames.includes(name)) balanceNames.push(name);
    }

    // A trailing spend of zero is nothing to report, so it is excluded like a balance
    // of zero: "$0.00 spent" would be pure noise.
    if (Number.isFinite(s.spend_usd) && s.spend_usd > 0) {
      spend += s.spend_usd;
      const name = nameOf(s.provider_id);
      if (!spendNames.includes(name)) spendNames.push(name);
    }
  }

  const advice = [];
  if (balance > 0) {
    advice.push({
      kind: "balance",
      text: `prepaid on ${joinNames(balanceNames)} — use it before paying elsewhere`,
      amount: balance,
      providers: balanceNames,
    });
  }
  if (spend > 0) {
    advice.push({
      kind: "spend",
      text: `spent ${spendNames.length === 1 ? "on" : "across"} ${joinNames(spendNames)} in the last 30 days`,
      amount: spend,
      providers: spendNames,
    });
  }
  return advice;
}
