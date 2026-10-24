import { Clock, Cpu, FileText } from 'lucide-react';

interface ProblemHeaderProps {
  id?: string;
  title: string;
  type: string;
  io: string;
  timeLimit: string;
  memoryLimit: string;
}

export function ProblemHeader({
  id,
  title,
  type,
  timeLimit,
  memoryLimit,
}: ProblemHeaderProps) {
  return (
    <div className="space-y-2">
      <h1 className="text-2xl font-bold tracking-tight">
        {id ? `${id}. ` : ''}
        {title}
      </h1>
      <div className="flex flex-wrap gap-2">
        <span className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium bg-pink-500/10 text-pink-600 dark:text-pink-400 ring-1 ring-pink-500/25">
          <FileText className="size-3" />
          {type}
        </span>
        <span className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium bg-amber-500/10 text-amber-600 dark:text-amber-400 ring-1 ring-amber-500/25">
          <Clock className="size-3" />
          {timeLimit}
        </span>
        <span className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium bg-blue-500/10 text-blue-600 dark:text-blue-400 ring-1 ring-blue-500/25">
          <Cpu className="size-3" />
          {memoryLimit}
        </span>
      </div>
    </div>
  );
}
