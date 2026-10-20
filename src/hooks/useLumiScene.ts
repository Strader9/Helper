// Tauri 适配层：mock useLumiScene
import { useState } from 'react';

export function useLumiScene() {
  const [scene, setScene] = useState('cosmos');
  return { scene, setScene, scenes: ['cosmos', 'office', 'sanctuary'] };
}
