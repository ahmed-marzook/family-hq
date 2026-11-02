import BarChartIcon from "@mui/icons-material/BarChart";
import GroupsIcon from "@mui/icons-material/Groups";
import HomeIcon from "@mui/icons-material/Home";
import PeopleIcon from "@mui/icons-material/People";
import { NavLink, Outlet } from "react-router";

import ApiStatus from "./ApiStatus";

const links = [
  { to: "/users", label: "Users", Icon: PeopleIcon },
  { to: "/groups", label: "Groups", Icon: GroupsIcon },
  { to: "/metrics", label: "Metrics", Icon: BarChartIcon },
];

export default function Layout() {
  return (
    <div className="min-h-dvh bg-stone-50 text-stone-900 md:flex dark:bg-stone-950 dark:text-stone-100">
      <aside className="flex flex-col gap-3 border-b border-stone-200 bg-white p-4 md:sticky md:top-0 md:h-dvh md:w-56 md:shrink-0 md:border-r md:border-b-0 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-center gap-2 font-semibold">
          <HomeIcon className="size-6 text-teal-700 dark:text-teal-400" />
          Family HQ Admin
        </div>
        <nav aria-label="Main" className="flex gap-1 md:flex-col">
          {links.map(({ to, label, Icon }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                `flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium ${
                  isActive
                    ? "bg-teal-50 text-teal-800 dark:bg-teal-950 dark:text-teal-300"
                    : "text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                }`
              }
            >
              <Icon className="size-5" />
              {label}
            </NavLink>
          ))}
        </nav>
        <ApiStatus />
      </aside>
      <main className="min-w-0 flex-1 p-4 md:p-8">
        <Outlet />
      </main>
    </div>
  );
}
