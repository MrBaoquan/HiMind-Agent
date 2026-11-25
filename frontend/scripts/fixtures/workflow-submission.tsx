import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { ConfirmProvider } from '../../src/components/ConfirmDialog';
import { ExtensionDevelopmentPage } from '../../src/pages/ExtensionDevelopmentPage';
import { agentApi } from '../../src/services/agentApi';
import '../../styles.css';

const stamp = '2026-11-23T01:00:00Z';
const id = 'com.himind.workflow.daily-work-log';
const manifest = { id, name: '每日工作日志', description: '生成工作日志', version: '0.10.6' };
const project = { id, kind: 'workflow', extension_id: id, name: manifest.name, description: manifest.description, version: manifest.version, workspace_path: 'F:/fixture/workflows/daily-work-log', workspace_available: true, source: 'local', source_repository: '', source_default_branch: 'main', source_subdirectory: '', source_commit: '', updated_at: stamp };
// Match the remote wire contract: there is no nested manifest or local state.
const submission = { id: 'workflowdraft_fixture', product_key: id, name: manifest.name, description: manifest.description, version: manifest.version, status: 'submitted', review_status: 'pending', artifact_id: 'artifact_fixture', release_id: 'release_fixture', release_status: 'draft', sha256: 'a'.repeat(64), created_at: stamp, updated_at: stamp };
const noop = async () => {};
agentApi.listBuiltinAiSessions = async () => [];
agentApi.extensionDevelopmentReadiness = async () => ({ kernel: { go: { installed: true } } });

function App() {
  const [items, setItems] = useState([]);
  const draft = { package_id: id, name: manifest.name, version: manifest.version, manifest, candidate_sha256: submission.sha256, state: items.length ? 'submitted' : 'confirmed', updated_at: stamp, submitted_at: items.length ? stamp : null, dashboard_draft_id: items.length ? submission.id : null };
  return <ConfirmProvider>
    <button onClick={() => setItems([submission])}>模拟提交结果</button>
    <button onClick={() => setItems([{ ...submission, status: 'approved', review_status: 'approved' }])}>模拟审核更新</button>
    <ExtensionDevelopmentPage dashboardEnabled workspace={{ configured: true, valid: true, root: 'F:/fixture', catalog_path: '', repository: '', default_branch: 'main', extension_count: 1, error: '' }} sources={[]} workspaces={[]} projectsError="" projects={[project]} remoteProjects={[]} pluginDrafts={[]} skillDrafts={[]} workflowDrafts={[draft]} expertDrafts={[]} instructionDrafts={[]} extensionActivations={[]} pluginSubmissions={[]} skillSubmissions={[]} workflowSubmissions={items} availablePlugins={[]} invitations={[]} accountAuthorized busyAction={null} loading={false} onRefresh={noop} onBuild={noop} onBuildWorkspace={noop} onCreate={noop} onOpenProject={noop} onAssociateProject={noop} onDevelopWithAi={noop} onDevelopWorkspaceWithAi={noop} onSubmit={noop} onPublishInstruction={noop} onOpenFolder={noop} onAddWorkspace={noop} onRemoveWorkspace={noop} onRemove={noop} onUpdateSource={noop} onSetDistributionTargets={noop} onAudienceUpdated={noop} onPublishDistribution={noop} onLoadCollaboration={noop} onSearchCollaborators={noop} onInviteCollaborator={noop} onRemoveCollaborator={noop} onRespondInvitation={noop} />
  </ConfirmProvider>;
}

createRoot(document.getElementById('root')).render(<App />);
