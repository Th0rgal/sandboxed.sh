'use client';

import { LibraryProvider } from '@/contexts/library-context';
import type { ReactNode } from 'react';

export default function LibraryLayout({ children }: { children: ReactNode }) {
  return <LibraryProvider>{children}</LibraryProvider>;
}
