import { Navigate, Route, Routes } from "react-router";

import Layout from "./components/Layout";
import Metrics from "./pages/Metrics";
import NotFound from "./pages/NotFound";
import User from "./pages/User";
import Users from "./pages/Users";

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={<Navigate to="/users" replace />} />
        <Route path="users" element={<Users />} />
        <Route path="users/:id" element={<User />} />
        <Route path="metrics" element={<Metrics />} />
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  );
}
