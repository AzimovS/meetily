import { useState, useEffect } from 'react';
import { Input } from './input';
import { Button } from './button';
import { Eye, EyeOff, Lock, Unlock } from 'lucide-react';

interface ApiKeyInputProps {
  value: string | null;
  onChange: (value: string) => void;
  onBlur?: (value: string) => void;
  placeholder?: string;
}

export function ApiKeyInput({
  value,
  onChange,
  onBlur,
  placeholder = 'Enter your API key',
}: ApiKeyInputProps) {
  const [showApiKey, setShowApiKey] = useState(false);
  const [isLocked, setIsLocked] = useState(!!value?.trim());
  const [isVibrating, setIsVibrating] = useState(false);

  // Auto-lock when value is set externally (e.g., fetched from DB)
  useEffect(() => {
    if (value?.trim()) {
      setIsLocked(true);
    }
  }, []);

  const handleLockedClick = () => {
    if (isLocked) {
      setIsVibrating(true);
      setTimeout(() => setIsVibrating(false), 500);
    }
  };

  return (
    <div className="relative mt-1">
      <Input
        type={showApiKey ? 'text' : 'password'}
        value={value || ''}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onBlur ? (e) => onBlur(e.target.value) : undefined}
        disabled={isLocked}
        placeholder={placeholder}
        className="pr-24"
      />
      {isLocked && value?.trim() && (
        <div
          onClick={handleLockedClick}
          className="absolute inset-0 flex items-center justify-center bg-muted/50 rounded-md cursor-not-allowed"
        />
      )}
      <div className="absolute inset-y-0 right-0 pr-1 flex items-center space-x-1">
        {value?.trim() && (
          <Button
            type="button"
            variant="ghost"
            size="icon"
            onClick={() => setIsLocked(!isLocked)}
            className={isVibrating ? 'animate-vibrate text-red-500' : ''}
            title={isLocked ? 'Unlock to edit' : 'Lock to prevent editing'}
          >
            {isLocked ? <Lock /> : <Unlock />}
          </Button>
        )}
        <Button
          type="button"
          variant="ghost"
          size="icon"
          onClick={() => setShowApiKey(!showApiKey)}
        >
          {showApiKey ? <EyeOff /> : <Eye />}
        </Button>
      </div>
    </div>
  );
}
