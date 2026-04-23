'use client';

import React from 'react';
import { ArrowLeft, Calendar as CalendarIcon } from 'lucide-react';
import { useRouter } from 'next/navigation';
import { ConnectCard } from '@/components/Calendar';

export default function CalendarPage() {
  const router = useRouter();

  return (
    <div className="h-screen bg-gray-50 flex flex-col">
      <div className="sticky top-0 z-10 bg-gray-50 border-b border-gray-200">
        <div className="max-w-4xl mx-auto px-8 py-6">
          <div className="flex items-center gap-4">
            <button
              type="button"
              onClick={() => router.back()}
              className="flex items-center gap-2 text-gray-600 hover:text-gray-900 transition-colors"
            >
              <ArrowLeft className="w-5 h-5" />
              <span>Back</span>
            </button>
            <div className="flex items-center gap-3">
              <CalendarIcon className="w-6 h-6 text-blue-600" />
              <h1 className="text-3xl font-bold">Calendar</h1>
            </div>
          </div>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-4xl mx-auto p-8 pt-6">
          <ConnectCard />
        </div>
      </div>
    </div>
  );
}
