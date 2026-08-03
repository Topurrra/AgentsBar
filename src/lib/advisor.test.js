// Run: node src/lib/advisor.test.js
import assert from "node:assert/strict";
import { costAdvice } from "./advisor.js";

// --- empty and no-dollar-data inputs ----------------------------------------------

// Nothing to say, so nothing is returned: the component renders no card at all.
assert.deepEqual(costAdvice([]), [], "no snapshots");
assert.deepEqual(costAdvice(undefined), [], "undefined is not a list");
assert.deepEqual(costAdvice(null), [], "null is not a list");
assert.deepEqual(costAdvice({}), [], "a non-array is not a list");
assert.deepEqual(costAdvice([null, undefined]), [], "holes are skipped");
assert.deepEqual(costAdvice([{}]), [], "a snapshot with no money fields");
assert.deepEqual(
  costAdvice([{ credits: 5 }]),
  [],
  "a unitless balance is not money",
);
assert.deepEqual(
  costAdvice([{ credits: 5, credits_unit: "CNY" }]),
  [],
  "a CNY wallet is not folded into dollar advice",
);

// --- USD balance detection ----------------------------------------------------------

// One provider with a USD prepaid balance is the headline advice.
{
  const advice = costAdvice([
    { provider_id: "deepseek", credits: 5.5, credits_unit: "USD" },
  ]);
  assert.equal(advice.length, 1);
  assert.deepEqual(advice[0], {
    kind: "balance",
    text: "prepaid on DeepSeek — use it before paying elsewhere",
    amount: 5.5,
    providers: ["DeepSeek"],
  });
}

// Several USD balances sum into one advice item naming every provider. The CNY wallet
// and the token count stay out of the total.
{
  const advice = costAdvice([
    { provider_id: "deepseek", credits: 20, credits_unit: "USD" },
    { provider_id: "openrouter", credits: 3.5, credits_unit: "USD" },
    { provider_id: "deepseek", credits: 99, credits_unit: "CNY" },
    { provider_id: "openai", credits: 100000 },
  ]);
  assert.equal(advice.length, 1);
  assert.equal(advice[0].kind, "balance");
  assert.equal(advice[0].amount, 23.5, "only USD balances sum");
  assert.deepEqual(advice[0].providers, ["DeepSeek", "OpenRouter"]);
  assert.equal(
    advice[0].text,
    "prepaid on DeepSeek and OpenRouter — use it before paying elsewhere",
  );
}

// Zero, negative and non-finite balances are nothing to spend, so they are excluded.
assert.deepEqual(
  costAdvice([{ provider_id: "xai", credits: 0, credits_unit: "USD" }]),
  [],
  "zero balance",
);
assert.deepEqual(
  costAdvice([{ provider_id: "xai", credits: -4, credits_unit: "USD" }]),
  [],
  "negative balance",
);
assert.deepEqual(
  costAdvice([{ provider_id: "xai", credits: NaN, credits_unit: "USD" }]),
  [],
  "NaN balance",
);
assert.deepEqual(
  costAdvice([{ provider_id: "xai", credits: Infinity, credits_unit: "USD" }]),
  [],
  "infinite balance",
);
assert.deepEqual(
  costAdvice([{ provider_id: "xai", credits: "20", credits_unit: "USD" }]),
  [],
  "a string balance is not a number",
);
// A zero balance next to a real one: only the real one counts.
{
  const advice = costAdvice([
    { provider_id: "xai", credits: 0, credits_unit: "USD" },
    { provider_id: "openrouter", credits: 7, credits_unit: "USD" },
  ]);
  assert.equal(advice.length, 1);
  assert.equal(advice[0].amount, 7);
  assert.deepEqual(advice[0].providers, ["OpenRouter"]);
}

// An id without a known display name falls back to the id itself, still legible.
{
  const advice = costAdvice([
    { provider_id: "futureprov", credits: 3, credits_unit: "USD" },
  ]);
  assert.deepEqual(advice[0].providers, ["futureprov"]);
  assert.match(advice[0].text, /futureprov/);
}

// --- spend observation ----------------------------------------------------------------

// Trailing 30-day spend surfaces on its own when a provider reports it.
{
  const advice = costAdvice([{ provider_id: "openai", spend_usd: 12.5 }]);
  assert.equal(advice.length, 1);
  assert.deepEqual(advice[0], {
    kind: "spend",
    text: "spent on OpenAI in the last 30 days",
    amount: 12.5,
    providers: ["OpenAI"],
  });
}

// Spends from several providers aggregate into one observation.
{
  const advice = costAdvice([
    { provider_id: "openai", spend_usd: 10 },
    { provider_id: "xai", spend_usd: 2.25 },
  ]);
  assert.equal(advice.length, 1);
  assert.equal(advice[0].amount, 12.25);
  assert.equal(advice[0].text, "spent across OpenAI and xAI in the last 30 days");
}

// Zero and non-finite spends are noise, not observations.
assert.deepEqual(
  costAdvice([{ provider_id: "openai", spend_usd: 0 }]),
  [],
  "zero spend",
);
assert.deepEqual(
  costAdvice([{ provider_id: "openai", spend_usd: NaN }]),
  [],
  "NaN spend",
);
assert.deepEqual(
  costAdvice([{ provider_id: "openai", spend_usd: -1 }]),
  [],
  "negative spend",
);

// --- order and shape ------------------------------------------------------------------

// The headline comes first: balance advice outranks the spend observation.
{
  const advice = costAdvice([
    { provider_id: "openai", spend_usd: 12.5 },
    { provider_id: "deepseek", credits: 4, credits_unit: "USD" },
  ]);
  assert.deepEqual(
    advice.map((a) => a.kind),
    ["balance", "spend"],
    "most important first",
  );
  assert.equal(advice[0].amount, 4);
  assert.equal(advice[1].amount, 12.5);
}

// Every item carries the full display shape, and never a pre-formatted dollar figure:
// locale formatting belongs to the component via `usd()`.
for (const a of costAdvice([
  { provider_id: "openai", spend_usd: 8, credits: 2, credits_unit: "USD" },
])) {
  assert.ok(a.kind === "balance" || a.kind === "spend");
  assert.equal(typeof a.text, "string");
  assert.ok(Number.isFinite(a.amount) && a.amount > 0, "amount is real, positive money");
  assert.ok(Array.isArray(a.providers) && a.providers.length > 0);
  assert.ok(!a.text.includes("$"), "the figure is formatted by the component, not here");
}

console.log("advisor.js ok");
