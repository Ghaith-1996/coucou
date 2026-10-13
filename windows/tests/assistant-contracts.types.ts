import { succeededReceipt, type ActionReceipt } from "../src/assistant/contracts";

// Checked by assistant-contracts.test.mjs; @ts-expect-error must be used, so
// weakening the receipt contract makes the ordinary test suite fail.
export function receiptTypeChecks(evidenceRef: string, receipt: ActionReceipt) {
  const success: ActionReceipt = succeededReceipt("act-1", evidenceRef);
  if (success.outcome === "succeeded") {
    const evidence: string = success.evidenceRef;
    void evidence;
  }

  const otherOutcomes: ActionReceipt[] = [
    { actionId: "act-1", outcome: "failed", errorCode: "failed" },
    { actionId: "act-1", outcome: "cancelled" },
    { actionId: "act-1", outcome: "unknown" },
    { actionId: "act-1", outcome: "unknown", evidenceRef: "partial" },
  ];

  // @ts-expect-error Success cannot omit its evidence.
  const missing: ActionReceipt = { actionId: "act-1", outcome: "succeeded" };
  // @ts-expect-error Success cannot contain undefined evidence.
  const undefinedEvidence: ActionReceipt = { actionId: "act-1", outcome: "succeeded", evidenceRef: undefined };
  // @ts-expect-error Empty evidence has not passed runtime validation.
  const empty: ActionReceipt = { actionId: "act-1", outcome: "succeeded", evidenceRef: "" };
  // @ts-expect-error Whitespace evidence has not passed runtime validation.
  const blank: ActionReceipt = { actionId: "act-1", outcome: "succeeded", evidenceRef: " \n" };
  // @ts-expect-error Even a nonblank literal must go through the constructor.
  const unchecked: ActionReceipt = { actionId: "act-1", outcome: "succeeded", evidenceRef: "observed" };
  // @ts-expect-error Dynamic strings might be blank; a string type is not validation.
  const dynamic: ActionReceipt = { actionId: "act-1", outcome: "succeeded", evidenceRef };
  // @ts-expect-error A caller cannot promote an arbitrary receipt to success.
  receipt.outcome = "succeeded";
  // @ts-expect-error A caller cannot replace validated evidence with a blank string.
  receipt.evidenceRef = "";
  // @ts-expect-error The constructor requires an evidence argument.
  succeededReceipt("act-1");

  void [otherOutcomes, missing, undefinedEvidence, empty, blank, unchecked, dynamic];
}
