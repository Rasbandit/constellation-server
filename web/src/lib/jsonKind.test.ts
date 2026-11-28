import { describe, expect, it } from 'vitest';
import { jsonKind } from './jsonKind';

const file = (text: string, name = 'x.json') => new File([text], name);

describe('jsonKind', () => {
  it('tells a manifest from a player file by its keys', async () => {
    expect(await jsonKind(file('{"game":"G","regions":[],"items":[]}'))).toBe('manifest');
    expect(await jsonKind(file('{"name":"Ann","game":"G","pool":["a"]}'))).toBe('player');
  });

  it('leaves YAML, broken JSON and unknown shapes alone', async () => {
    expect(await jsonKind(file('name: Ann', 'a.yaml'))).toBe('other');
    expect(await jsonKind(file('{'))).toBe('other');
    expect(await jsonKind(file('[]'))).toBe('other');
    expect(await jsonKind(file('{"x":1}'))).toBe('other');
  });
});
