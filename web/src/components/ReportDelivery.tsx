import { ApiError, type Delivery, type ReportFeature } from "../api";
import { useChange, type Owner } from "../useChange";

const choices: { value: Delivery; label: string; groupOnly?: boolean }[] = [
  { value: "here", label: "In the chat" },
  { value: "dm", label: "Each member privately", groupOnly: true },
  { value: "off", label: "Off" },
];

/**
 * Where one feature's reports go, as `/prayers reports here|dm|off` sets it
 * in WhatsApp. Works for any feature with reports: pass its key.
 */
export default function ReportDelivery({
  owner,
  feature,
  delivery,
  isGroup,
}: {
  owner: Owner;
  feature: ReportFeature;
  delivery: Delivery;
  isGroup: boolean;
}) {
  const change = useChange(owner);
  const shown = choices.filter((c) => isGroup || !c.groupOnly);

  return (
    <div>
      <fieldset disabled={change.isPending}>
        <legend className="mb-1.5 text-sm text-stone-600 dark:text-stone-400">
          Reports go
        </legend>
        <div className="inline-flex flex-wrap rounded-md border border-stone-300 dark:border-stone-700">
          {shown.map((choice) => (
            <label
              key={choice.value}
              className={`cursor-pointer px-3 py-1.5 text-sm first:rounded-l-md last:rounded-r-md has-focus-visible:outline-2 has-focus-visible:outline-teal-600 ${
                choice.value === delivery
                  ? "bg-teal-600 text-white"
                  : "bg-white hover:bg-stone-50 dark:bg-stone-900 dark:hover:bg-stone-800"
              }`}
            >
              <input
                type="radio"
                name={`${owner.path}-${feature}-reports`}
                value={choice.value}
                checked={choice.value === delivery}
                onChange={() =>
                  change.mutate({
                    method: "POST",
                    path: `/reports/${feature}`,
                    body: { delivery: choice.value },
                  })
                }
                className="sr-only"
              />
              {choice.label}
            </label>
          ))}
        </div>
      </fieldset>
      {change.error && (
        <p role="alert" className="mt-1 text-sm text-red-700 dark:text-red-400">
          {change.error instanceof ApiError && change.error.reason
            ? change.error.reason
            : "Couldn't save that."}
        </p>
      )}
    </div>
  );
}
