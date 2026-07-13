import React from 'react';

// login button
export const LoginButton = ({ onClick }: { onClick: () => void }) => {
  return <button onClick={onClick}>Login</button>;
};

export default function Page() {
  return <LoginButton onClick={() => login()} />;
}
