import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Input } from './ui/input';
import { Label } from './ui/label';
import { Button } from './ui/button';
import { ModelManager } from './WhisperModelManager';
import { ParakeetModelManager } from './ParakeetModelManager';
import { cn } from '@/lib/utils';
import { toast } from 'sonner';


export interface TranscriptModelProps {
    provider: 'localWhisper' | 'parakeet' | 'remote' | 'deepgram' | 'elevenLabs' | 'groq' | 'openai';
    model: string;
    endpointUrl?: string | null;
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
    const [uiEndpointUrl, setUiEndpointUrl] = useState(transcriptModelConfig.endpointUrl || '');
    const [uiApiKey, setUiApiKey] = useState<string | null>(transcriptModelConfig.apiKey || null);
    const [apiKeyDirty, setApiKeyDirty] = useState(false);
    const [isSaving, setIsSaving] = useState(false);

    const isRemoteProvider = !LOCAL_PROVIDERS.has(uiProvider);
    const requiresApiKey = isRemoteProvider;

    const isDoneDisabled =
        (uiProvider === 'remote' && !uiEndpointUrl?.trim()) ||
        (uiProvider === 'remote' && !uiModel?.trim()) ||
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
                endpointUrl: uiProvider === 'remote' ? uiEndpointUrl?.trim() || null : null,
                apiKey: uiApiKey?.trim() || null,
            };
            await invoke('api_save_transcript_config', {
                provider: config.provider,
                model: config.model,
                endpointUrl: config.endpointUrl,
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

                                if (provider === 'remote') {
                                    const existingUrl = transcriptModelConfig.provider === 'remote'
                                        ? (transcriptModelConfig.endpointUrl || '') : '';
                                    const existingModel = transcriptModelConfig.provider === 'remote'
                                        ? transcriptModelConfig.model : '';
                                    setUiEndpointUrl(existingUrl);
                                    setUiModel(existingModel);
                                    fetchApiKey('remote');
                                } else if (LOCAL_PROVIDERS.has(provider)) {
                                    const existingModel = transcriptModelConfig.provider === provider
                                        ? transcriptModelConfig.model : '';
                                    setUiModel(existingModel);
                                    setUiEndpointUrl('');
                                }
                            }}
                        >
                            <SelectTrigger>
                                <SelectValue placeholder="Select provider" />
                            </SelectTrigger>
                            <SelectContent>
                                <SelectItem value="parakeet">Parakeet (Recommended - Real-time / Accurate)</SelectItem>
                                <SelectItem value="localWhisper">Local Whisper (High Accuracy)</SelectItem>
                                <SelectItem value="remote">Remote Transcription</SelectItem>
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

                {uiProvider === 'remote' && (
                    <>
                        <div>
                            <Label>Endpoint URL</Label>
                            <Input
                                type="url"
                                className="mt-1"
                                value={uiEndpointUrl}
                                onChange={(e) => setUiEndpointUrl(e.target.value)}
                                placeholder="e.g. https://your-server/v1/audio/transcriptions"
                            />
                            <p className="text-xs text-muted-foreground mt-1">
                                The full URL of your OpenAI-compatible transcription endpoint
                            </p>
                        </div>
                        <div>
                            <Label>Model Name</Label>
                            <Input
                                className="mt-1"
                                value={uiModel}
                                onChange={(e) => setUiModel(e.target.value)}
                                placeholder="e.g. whisper-1, whisper-large-v3"
                                maxLength={256}
                            />
                            <p className="text-xs text-muted-foreground mt-1">
                                The model identifier to send with transcription requests
                            </p>
                        </div>
                    </>
                )}

                {requiresApiKey && (
                    <div>
                        <Label>API Key</Label>
                        <Input
                            type="password"
                            value={uiApiKey || ''}
                            onChange={(e) => {
                                setUiApiKey(e.target.value);
                                setApiKeyDirty(true);
                            }}
                            placeholder="Enter your API key"
                            className="mt-1"
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
