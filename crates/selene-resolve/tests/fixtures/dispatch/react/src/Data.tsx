import { createBrowserRouter } from 'react-router-dom';
import Article from './Article';

export const router = createBrowserRouter([
  { path: '/data/article', element: <Article /> },
]);
