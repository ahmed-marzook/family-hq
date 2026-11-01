import { Link } from "react-router";

import Page from "../components/Page";

export default function NotFound() {
  return (
    <Page title="Page not found">
      <Link
        to="/users"
        className="text-teal-700 underline dark:text-teal-400"
      >
        Go to Users
      </Link>
    </Page>
  );
}
