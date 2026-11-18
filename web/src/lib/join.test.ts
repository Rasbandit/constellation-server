import { describe, expect, it } from 'vitest';
import { joinPath } from './join';

describe('joinPath', () => {
  it('turns a bare code or a short link into the code route', () => {
    expect(joinPath('k7q2xm')).toBe('/c/K7Q2XM');
    expect(joinPath('https://const.gg/c/K7Q2XM')).toBe('/c/K7Q2XM');
  });
  it('keeps room links and room ids on the room route', () => {
    expect(joinPath('http://localhost:5173/rooms/friday-7k2q?x=1')).toBe('/rooms/friday-7k2q');
    expect(joinPath('friday-7k2q')).toBe('/rooms/friday-7k2q');
  });
  it('refuses empty input', () => {
    expect(joinPath('   ')).toBeNull();
  });
});
