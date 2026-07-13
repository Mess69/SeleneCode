import { Router } from 'express';
import { login } from './service';
import { auth } from './middleware';

const router = Router();

// router.get('/dead', deadHandler);
router.post('/users/login', async (req, res) => {
  const user = await login(req.body);
  res.json(user);
});

router.get('/profile', auth, getProfile);

router.use('/api', apiRouter);

export default router;
