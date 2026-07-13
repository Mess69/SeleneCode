import { Routes, Route } from 'react-router-dom';
import Article from './Article';

export default function App() {
  return (
    <Routes>
      <Route path="/article/:slug" element={<Article />} />
      <Route element={<NoPath />} />
    </Routes>
  );
}
