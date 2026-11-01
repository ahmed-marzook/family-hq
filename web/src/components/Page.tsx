import type { ReactNode } from "react";

/** A page's heading and contents. */
export default function Page({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <>
      <h1 className="mb-4 text-2xl font-semibold">{title}</h1>
      {children}
    </>
  );
}
