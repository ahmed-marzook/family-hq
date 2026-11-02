/** One column: its value, the label under the axis (if any), and the text
 * its tooltip and screen readers get. */
export type Column = { value: number; tick?: string; label: string };

/**
 * A single-series column chart in plain HTML: thin bars with rounded tops
 * and a 2px gap, anchored to the baseline, a tooltip per bar (hover or
 * keyboard focus), a recessive axis, and a table of the same numbers.
 */
export default function ColumnChart({
  title,
  columns,
  unit,
}: {
  title: string;
  columns: Column[];
  /** "uses", for the tooltip and the table's heading. */
  unit: string;
}) {
  const max = Math.max(1, ...columns.map((c) => c.value));

  return (
    <figure>
      <figcaption className="sr-only">{title}</figcaption>
      <div className="flex gap-2">
        {/* The scale: just the top value and zero. */}
        <div className="flex h-40 flex-col justify-between text-right text-xs text-stone-500 tabular-nums">
          <span>{max}</span>
          <span>0</span>
        </div>
        <div className="min-w-0 flex-1">
          <div className="relative flex h-40 items-end gap-0.5 border-b border-stone-300 dark:border-stone-700">
            {/* A faint line at the top value. */}
            <div className="pointer-events-none absolute inset-x-0 top-0 border-t border-dashed border-stone-200 dark:border-stone-800" />
            {columns.map((column, i) => (
              <div
                key={i}
                tabIndex={0}
                aria-label={`${column.label}: ${column.value} ${unit}`}
                // The hit area is the whole column height, not just the bar.
                className="group relative flex h-full min-w-0 flex-1 items-end outline-none"
              >
                <div
                  className="w-full rounded-t bg-teal-600 group-hover:bg-teal-700 group-focus-visible:bg-teal-700 dark:group-hover:bg-teal-500 dark:group-focus-visible:bg-teal-500"
                  style={{
                    height: `${(column.value / max) * 100}%`,
                    // A zero still shows where the bar would be.
                    minHeight: column.value > 0 ? 2 : 0,
                  }}
                />
                <div
                  role="tooltip"
                  className={`pointer-events-none absolute bottom-full z-10 mb-1 hidden rounded-md bg-stone-900 px-2 py-1 text-xs whitespace-nowrap text-white shadow group-hover:block group-focus-visible:block dark:bg-stone-100 dark:text-stone-900 ${
                    i > columns.length / 2 ? "right-0" : "left-0"
                  }`}
                >
                  <span className="font-medium tabular-nums">{column.value}</span> {unit}
                  <span className="block text-stone-300 dark:text-stone-600">{column.label}</span>
                </div>
              </div>
            ))}
          </div>
          <div className="mt-1 flex gap-0.5 text-xs text-stone-500">
            {columns.map((column, i) => (
              // Each label hangs off its column: from the left edge, or in
              // the last part of the chart from the right, so it never
              // runs past the end.
              <span key={i} className="relative h-4 min-w-0 flex-1">
                {column.tick && (
                  <span
                    className={`absolute top-0 whitespace-nowrap ${
                      i > columns.length * 0.8 ? "right-0" : "left-0"
                    }`}
                  >
                    {column.tick}
                  </span>
                )}
              </span>
            ))}
          </div>
        </div>
      </div>
      <details className="mt-2 text-sm">
        <summary className="cursor-pointer text-stone-600 dark:text-stone-400">
          Show as a table
        </summary>
        <table className="mt-2 text-sm">
          <thead>
            <tr className="text-left text-stone-600 dark:text-stone-400">
              <th className="pr-6 font-medium">When</th>
              <th className="text-right font-medium capitalize">{unit}</th>
            </tr>
          </thead>
          <tbody className="tabular-nums">
            {columns.map((column, i) => (
              <tr key={i}>
                <td className="pr-6">{column.label}</td>
                <td className="text-right">{column.value}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
    </figure>
  );
}
