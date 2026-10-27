// Tauri 适配层：mock useEAssistantScene
import { useState } from 'react';

export function useEAssistantScene() {
  const [scene, setScene] = useState('cosmos');
  return { scene, setScene, scenes: ['cosmos', 'office', 'sanctuary'] };
}
