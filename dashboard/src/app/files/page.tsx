'use client';

import { redirect } from 'next/navigation';

export default function FilesPage() {
  // Redirect to console which has the files tab
  redirect('/console');
}
