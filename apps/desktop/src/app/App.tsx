import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "../components/AppShell";
import { ActivityView } from "../views/ActivityView";
import { AutomationsView } from "../views/AutomationsView";
import { ConversationView } from "../views/ConversationView";
import { ExtensionsView } from "../views/ExtensionsView";
import { HomeView } from "../views/HomeView";
import { LibraryView } from "../views/LibraryView";
import { ProjectsView } from "../views/ProjectsView";
import { SettingsView } from "../views/SettingsView";
import { SetupView } from "../views/SetupView";

export function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<HomeView />} />
        <Route path="projects" element={<ProjectsView />} />
        <Route path="projects/:projectId" element={<ProjectsView />} />
        <Route path="activity" element={<ActivityView />} />
        <Route path="conversations/:threadId" element={<ConversationView />} />
        <Route path="library" element={<LibraryView />} />
        <Route path="automations" element={<AutomationsView />} />
        <Route path="extensions" element={<ExtensionsView />} />
        <Route path="settings" element={<SettingsView />} />
        <Route path="setup" element={<SetupView />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
