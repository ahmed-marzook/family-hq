import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";

import { getJson, type Metrics as MetricsData } from "../api";
import LoadError from "../components/LoadError";
import { MetricsReport, PeriodPicker, metricsPath } from "../components/MetricsReport";
import Page from "../components/Page";

export default function Metrics() {
  // The period lives in the address (`?days=7`), so a reload or a shared
  // link shows the same view. No `days` means 30.
  const [params, setParams] = useSearchParams();
  const days = params.get("days") ?? "30";

  const { data, error, isPending } = useQuery({
    queryKey: ["metrics", days],
    queryFn: () => getJson<MetricsData>(metricsPath(days)),
  });

  return (
    <Page title="Metrics">
      <PeriodPicker days={days} onChange={(d) => setParams(d === "30" ? {} : { days: d })} />
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} />}
      {data && <MetricsReport data={data} scope="all" />}
    </Page>
  );
}
