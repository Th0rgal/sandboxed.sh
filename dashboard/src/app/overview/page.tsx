'use client';

import { redirect } from 'next/navigation';

export default function OverviewPage() {
  // Overview moved to the root route.
  redirect('/');
}
