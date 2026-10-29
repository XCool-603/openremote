import React from 'react';
import { Routes, Route } from 'react-router-dom';
import { Sidebar } from './components/Sidebar';
import { Home } from './pages/Home';
import { RemoteView } from './pages/RemoteView';
import { FileTransfer } from './pages/FileTransfer';
import { Settings } from './pages/Settings';

export const App: React.FC = () => {
  return (
    <div className="flex h-screen w-screen bg-surface-dark text-white overflow-hidden">
      <Sidebar />
      <main className="flex-1 flex flex-col h-full overflow-hidden relative">
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/remote" element={<RemoteView />} />
          <Route path="/files" element={<FileTransfer />} />
          <Route path="/settings" element={<Settings />} />
        </Routes>
      </main>
    </div>
  );
};

export default App;
