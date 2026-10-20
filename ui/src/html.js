import { h } from 'preact';
import htm from 'htm';

export const html = htm.bind(h);
export const cx = (...names) => names.filter(Boolean).join(' ');
