import { serviceCapabilities } from '../src/pages/aiServiceCapabilities.ts';
const service=(id, model='', media={})=>({id,display_name:id,base_url:'https://'+id+'.example/v1',protocol:'openai-responses',model,models:model?[model]:[],image_model:'',image_adapter:'',media,created_at:'',updated_at:''});
const videoOnly=service('video-only','',{'video.generate':{adapter:'ark-video',model:'seedance-test'}});
console.log(JSON.stringify(serviceCapabilities(videoOnly)));
