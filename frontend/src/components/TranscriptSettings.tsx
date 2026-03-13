import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Input } from './ui/input';
import { Label } from './ui/label';
import { Button } from './ui/button';
import { ApiKeyInput } from './ui/ApiKeyInput';
import { ModelManager } from './WhisperModelManager';
import { ParakeetModelManager } from './ParakeetModelManager';
import { cn } from '@/lib/utils';
import { toast } from 'sonner';


export interface TranscriptModelProps {
    provider: 'localWhisper' | 'parakeet' | 'voxtral' | 'deepgram' | 'elevenLabs' | 'groq' | 'openai';
    model: string;
    apiKey?: string | null;
}

export interface TranscriptSettingsProps {
    transcriptModelConfig: TranscriptModelProps;
    setTranscriptModelConfig: (config: TranscriptModelProps) => void;
    onModelSelect?: () => void;
}

const LOCAL_PROVIDERS = new Set<TranscriptModelProps['provider']>(['localWhisper', 'parakeet']);

export function TranscriptSettings({ transcriptModelConfig, setTranscriptModelConfig, onModelSelect }: TranscriptSettingsProps) {
    // Local draft state -- only pushed to context on Save (remote) or model select (local)
    const [uiProvider, setUiProvider] = useState<TranscriptModelProps['provider']>(transcriptModelConfig.provider);
    const [uiModel, setUiModel] = useState(transcriptModelConfig.model);
    const [uiApiKey, setUiApiKey] = useState<string | null>(transcriptModelConfig.apiKey || null);
    const [apiKeyDirty, setApiKeyDirty] = useState(false);
    const [isSaving, setIsSaving] = useState(false);

    const isRemoteProvider = !LOCAL_PROVIDERS.has(uiProvider);
    const requiresApiKey = isRemoteProvider;

    const isDoneDisabled =
        (isRemoteProvider && !uiModel?.trim()) ||
        (requiresApiKey && !uiApiKey?.trim());

    const fetchApiKey = async (provider: TranscriptModelProps['provider']) => {
        try {
            const data = await invoke('api_get_transcript_api_key', { provider }) as string;
            if (!apiKeyDirty) {
                setUiApiKey(data || '');
            }
        } catch (err) {
            console.error('Error fetching API key:', err);
            if (!apiKeyDirty) {
                setUiApiKey(null);
            }
        }
    };

    const handleSave = async () => {
        if (isSaving) return;
        setIsSaving(true);
        try {
            const config: TranscriptModelProps = {
                provider: uiProvider,
                model: uiModel,
                apiKey: uiApiKey?.trim() || null,
            };
            await invoke('api_save_transcript_config', {
                provider: config.provider,
                model: config.model,
                apiKey: config.apiKey,
            });

            // Only update context after successful persist
            setTranscriptModelConfig(config);
            toast.success('Transcription settings saved');
        } catch (err) {
            console.error('[TranscriptSettings] Failed to save:', err);
            toast.error('Failed to save transcription settings');
        } finally {
            setIsSaving(false);
        }
    };

    const handleWhisperModelSelect = (modelName: string) => {
        const config: TranscriptModelProps = { provider: 'localWhisper', model: modelName, apiKey: null };
        setUiModel(modelName);
        setTranscriptModelConfig(config);
        if (onModelSelect) onModelSelect();
    };

    const handleParakeetModelSelect = (modelName: string) => {
        const config: TranscriptModelProps = { provider: 'parakeet', model: modelName, apiKey: null };
        setUiModel(modelName);
        setTranscriptModelConfig(config);
        if (onModelSelect) onModelSelect();
    };

    return (
        <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
            <div className="flex justify-between items-center mb-4">
                <h3 className="text-lg font-semibold">Transcription Settings</h3>
            </div>

            <div className="space-y-4">
                <div>
                    <Label>Transcription Model</Label>
                    <div className="flex space-x-2 mt-1">
                        <Select
                            value={uiProvider}
                            onValueChange={(value) => {
                                const provider = value as TranscriptModelProps['provider'];
                                setUiProvider(provider);
                                setUiApiKey(null);
                                setApiKeyDirty(false);

                                if (provider === 'voxtral') {
                                    const existingUrl = transcriptModelConfig.provider === 'voxtral'
                                        ? transcriptModelConfig.model : '';
                                    setUiModel(existingUrl);
                                    fetchApiKey('voxtral');
                                } else if (LOCAL_PROVIDERS.has(provider)) {
                                    const existingModel = transcriptModelConfig.provider === provider
                                        ? transcriptModelConfig.model : '';
                                    setUiModel(existingModel);
                                }
                            }}
                        >
                            <SelectTrigger>
                                <SelectValue placeholder="Select provider" />
                            </SelectTrigger>
                            <SelectContent>
                                <SelectItem value="parakeet">Parakeet (Recommended - Real-time / Accurate)</SelectItem>
                                <SelectItem value="localWhisper">Local Whisper (High Accuracy)</SelectItem>
                                <SelectItem value="voxtral">Voxtral (Remote)</SelectItem>
                            </SelectContent>
                        </Select>
                    </div>
                </div>

                {uiProvider === 'localWhisper' && (
                    <div>
                        <ModelManager
                            selectedModel={transcriptModelConfig.provider === 'localWhisper' ? transcriptModelConfig.model : undefined}
                            onModelSelect={handleWhisperModelSelect}
                            autoSave={true}
                        />
                    </div>
                )}

                {uiProvider === 'parakeet' && (
                    <div>
                        <ParakeetModelManager
                            selectedModel={transcriptModelConfig.provider === 'parakeet' ? transcriptModelConfig.model : undefined}
                            onModelSelect={handleParakeetModelSelect}
                            autoSave={true}
                        />
                    </div>
                )}

                {uiProvider === 'voxtral' && (
                    <div>
                        <Label>Endpoint URL</Label>
                        <Input
                            type="url"
                            className="mt-1"
                            value={uiModel}
                            onChange={(e) => setUiModel(e.target.value)}
                            placeholder="e.g. https://your-server/v1/audio/transcriptions"
                        />
                        <p className="text-xs text-muted-foreground mt-1">
                            The full URL of your Voxtral-compatible transcription endpoint
                        </p>
                    </div>
                )}

                {requiresApiKey && (
                    <div>
                        <Label>API Key</Label>
                        <ApiKeyInput
                            value={uiApiKey}
                            onChange={(value) => {
                                setUiApiKey(value);
                                setApiKeyDirty(true);
                            }}
                        />
                    </div>
                )}

                {isRemoteProvider && (
                    <div className="mt-6 flex justify-end">
                        <Button
                            className={cn(
                                'px-4 text-sm font-medium text-white rounded-md focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-blue-500',
                                isDoneDisabled || isSaving ? 'bg-gray-400 cursor-not-allowed' : 'bg-blue-600 hover:bg-blue-700'
                            )}
                            onClick={handleSave}
                            disabled={isDoneDisabled || isSaving}
                        >
                            {isSaving ? 'Saving...' : 'Save'}
                        </Button>
                    </div>
                )}
            </div>
        </div>
    );
}
